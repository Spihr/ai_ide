//! 内核日志主动上报（`core.log` 事件）：把 tracing 事件抬成对外壳的单向通知。
//!
//! ## 为什么还要这一条通道
//!
//! 内核日志此前有两条出口：stderr（主进程按行读后落 `core-shell-*.log`）与
//! `<data_dir>/logs/core-*.jsonl`（内核自己写）。两条都是**文件**：外壳要显示「内核
//! 刚才说了什么」，只能去追文件尾巴或猜 stderr 的时机。协议里早有
//! `ai_ide_protocol::methods::event::LOG`（`core.log`）与
//! [`crate::peer::Peer::forward_log`]，但全仓没有调用点——事件面定好了、线没接。
//!
//! 这里补的就是那根线：
//!
//! ```text
//! tracing 事件 ─┬─→ stderr
//!               ├─→ core-*.jsonl
//!               └─→ CoreLogLayer ─→ 通道 ─→ pump ─→ core.log 通知（stdout 帧）
//! ```
//!
//! 落盘那份是**排查内核自身**的，事件这份是给**外壳实时显示**的；同一句话走两条
//! 路不冲突，但内容必须一致：两侧都过同一套脱敏规则（`ai_ide_store::redact_text`，
//! `AI_IDE_LOG_REDACT=off` 时两边一起关）。
//!
//! ## 三条硬约束
//!
//! 1. **不能形成反馈回路**。上报路径自己出错时若不设防，就会「上报失败 → 打日志 →
//!    再上报 → 再失败」无限循环。三重防线：
//!    - 事件投递期间置线程本地闸门（[`Pushing`]），同一线程内的重入事件直接丢弃；
//!    - 目标名以 [`PUSH_TARGET`] 开头的事件一律不上报（本模块自己的日志）；
//!    - [`crate::peer`] 自身的日志不上报——它的 error 正是「写 stdout 失败」，
//!      把这句话再写回 stdout 只会把失败放大。
//! 2. **脱敏在出口做**。与 JSONL 出口共用 `redact_text`：日志文件里没有明文密钥，
//!    事件里也不该有。
//! 3. **异步、不阻塞、不 panic**。tracing 的 `on_event` 是同步的，而写帧要 await；
//!    所以这里只往无界通道里塞一条，由 [`pump`] 在 tokio 任务里转发。通道满了不是
//!    问题（无界），但单条消息按 [`MAX_MESSAGE_CHARS`] 截断，防止一条巨型日志把
//!    stdout 帧撑爆。
//!
//! ## 开关
//!
//! | 环境变量 | 默认 | 作用 |
//! |---|---|---|
//! | `AI_IDE_CORE_LOG_PUSH` | `on` | 设为 `off` / `0` / `false` / `no` 则不上报（只留文件出口） |
//!
//! 开关只在**投递时**读，不给测试留后门：单测用 [`install_sink`] 自己装 sink，
//! 不依赖环境变量。

use std::sync::{Arc, Mutex, OnceLock};

use ai_ide_store::redact_text;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use crate::peer::Peer;

/// 上报开关（`off` 关闭）。
pub const CORE_LOG_PUSH_ENV: &str = "AI_IDE_CORE_LOG_PUSH";

/// 本模块的 tracing 目标；以它开头的事件不再上报（防自激）。
pub const PUSH_TARGET: &str = "ai_ide_core::core_log";

/// 单条消息的字符上限（超长截断并加省略标记）。
///
/// 4 KiB 足够装下常规诊断（路径、错误原文、插件 id），又远小于协议帧的常规尺寸。
pub const MAX_MESSAGE_CHARS: usize = 4 * 1024;

/// 一条待上报的内核日志。
///
/// 字段与 `core.log` 事件载荷一一对应（`level` / `target` / `message`，见 docs/03），
/// 外壳侧 `formatCoreLogNotification` 就是按这三个键拼人话的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreLogRecord {
    /// 小写级别（`error` / `warn` / `info` / `debug` / `trace`）。
    pub level: String,
    /// tracing 目标，如 `ai_ide_core::dispatch`。
    pub target: String,
    /// 已脱敏、已截断的单行正文。
    pub message: String,
}

impl CoreLogRecord {
    /// 从 tracing 事件构造（脱敏 + 截断都在这里）。
    pub fn from_event(event: &Event<'_>, redact: bool) -> Option<Self> {
        let metadata = event.metadata();
        if is_internal_target(metadata.target()) {
            return None;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let raw = visitor.into_message();
        if raw.is_empty() {
            return None;
        }
        let text = if redact { redact_text(&raw) } else { raw };
        Some(Self {
            level: level_name(metadata.level()).to_string(),
            target: metadata.target().to_string(),
            message: truncate(text, MAX_MESSAGE_CHARS),
        })
    }
}

/// 落点：一个「收到记录就做点什么」的回调。
///
/// 用回调而不是直接绑通道，是为了让测试能观察**投递本身**（含「回调里再打日志」
/// 这种最容易写出反馈回路的用法），而生产侧只是把记录塞进通道。
pub type CoreLogSink = Arc<dyn Fn(CoreLogRecord) + Send + Sync>;

fn sink_slot() -> &'static Mutex<Option<CoreLogSink>> {
    static SLOT: OnceLock<Mutex<Option<CoreLogSink>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// 装一个上报落点（重复调用会替换旧的）。
///
/// 返回 `true` 表示本次是新装；`false` 表示覆盖了旧落点——`run()` 只会装一次，
/// 这个返回值主要给测试与诊断用。
pub fn install_sink(sink: CoreLogSink) -> bool {
    let mut slot = match sink_slot().lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
    };
    let fresh = slot.is_none();
    *slot = Some(sink);
    fresh
}

/// 摘掉落点：之后的事件只走 stderr 与 JSONL（进程退出或测试收尾用）。
pub fn clear_sink() {
    let mut slot = match sink_slot().lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
    };
    *slot = None;
}

/// 是否已装落点（没装时 `on_event` 立刻返回，不产生任何开销以外的行为）。
pub fn has_sink() -> bool {
    let slot = match sink_slot().lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
    };
    slot.is_some()
}

/// 上报是否开启（`AI_IDE_CORE_LOG_PUSH=off` 可关）。
pub fn push_enabled() -> bool {
    match std::env::var(CORE_LOG_PUSH_ENV) {
        Ok(value) => {
            let lowered = value.trim().to_ascii_lowercase();
            !matches!(lowered.as_str(), "" | "0" | "off" | "false" | "no")
        }
        Err(_) => true,
    }
}

/// 目标是否属于「不该上报」的自家模块。
///
/// `peer` 在这里是**防回路的关键一环**：`peer` 的 error 只有一种来源——写 stdout /
/// 序列化失败，而这正是上报本身会触发的失败。把这句话再写回 stdout，等于在失败
/// 时自我放大；留在 stderr 与文件里给人看就够了。
fn is_internal_target(target: &str) -> bool {
    target == PUSH_TARGET
        || target.starts_with(&format!("{PUSH_TARGET}::"))
        || target == "ai_ide_core::peer"
        || target.starts_with("ai_ide_core::peer::")
}

/// 事件投递期间的线程本地闸门。
///
/// `on_event` 里调 sink，sink 又打日志的话会**同步重入** `on_event`——闸门就是为此
/// 存在。用线程本地而不是全局原子：其它线程的正常日志不该被这一次投递连坐。
struct Pushing(bool);

impl Pushing {
    fn enter() -> Self {
        let previous = IN_PUSH.with(|flag| {
            let previous = flag.get();
            flag.set(true);
            previous
        });
        Self(previous)
    }
}

impl Drop for Pushing {
    fn drop(&mut self) {
        if !self.0 {
            IN_PUSH.with(|flag| flag.set(false));
        }
    }
}

thread_local! {
    static IN_PUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 把事件投给当前落点；未装落点 / 开关关 / 正在投递中，一律静默丢弃。
fn dispatch(record: CoreLogRecord) {
    let still_active = IN_PUSH.with(|flag| flag.get());
    if still_active {
        return;
    }
    let sink = {
        let slot = match sink_slot().lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        match slot.as_ref() {
            Some(sink) => Arc::clone(sink),
            None => return,
        }
    };
    let _guard = Pushing::enter();
    sink(record);
}

/// 上报 layer：挂在既有 registry 上，与 stderr / JSONL 两条 layer 平级。
pub struct CoreLogLayer {
    redact: bool,
}

impl CoreLogLayer {
    /// 按环境变量里的脱敏开关构造（与 JSONL 出口同口径）。
    pub fn new(redact: bool) -> Self {
        Self { redact }
    }

    /// 是否脱敏。
    pub fn redact(&self) -> bool {
        self.redact
    }
}

impl Default for CoreLogLayer {
    fn default() -> Self {
        Self::new(crate::logging::redact_enabled())
    }
}

impl<S: Subscriber> Layer<S> for CoreLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !push_enabled() || !has_sink() {
            return;
        }
        if let Some(record) = CoreLogRecord::from_event(event, self.redact) {
            dispatch(record);
        }
    }
}

/// 通道接收端 + 对端：把记录逐条抬成 `core.log` 通知。
///
/// 顺序转发（不并发）：帧本身的顺序就是日志顺序，乱序会让「先失败后重试」这类
/// 因果在面板上读不出来。
pub async fn pump(peer: Arc<Peer>, mut receiver: UnboundedReceiver<CoreLogRecord>) {
    while let Some(record) = receiver.recv().await {
        peer.forward_log(&record.level, &record.target, &record.message)
            .await;
    }
}

/// 装一个通道落点并返回接收端（`run()` 用；测试也可以直接用它断言端到端）。
pub fn install_channel() -> UnboundedReceiver<CoreLogRecord> {
    let (sender, receiver) = unbounded_channel();
    install_sink(Arc::new(move |record| {
        // 接收端被丢了（pump 结束）就不必再投：send 失败即静默，日志不该反过来报错。
        let _ = sender.send(record);
    }));
    receiver
}

/// `tracing::Level` → 小写名字（外壳按 `[level] target: message` 拼人话）。
fn level_name(level: &tracing::Level) -> &'static str {
    match *level {
        tracing::Level::ERROR => "error",
        tracing::Level::WARN => "warn",
        tracing::Level::INFO => "info",
        tracing::Level::DEBUG => "debug",
        tracing::Level::TRACE => "trace",
    }
}

/// 按字符边界截断，超长时留省略标记。
fn truncate(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 事件字段收集器：`message` 是正文，其余字段按 `key=value` 追加在后面。
///
/// 与 JSONL 出口（`logging::JsonVisitor`）收集同一批字段，只是这里要的是**一行人话**，
/// 所以把 `error = ...` / `plugin_id = ...` 这类上下文拼进正文，而不是丢掉。
#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    extras: Vec<String>,
}

impl MessageVisitor {
    fn push(&mut self, field: &Field, rendered: String) {
        if field.name() == "message" {
            self.message = Some(rendered);
        } else {
            self.extras.push(format!("{}={rendered}", field.name()));
        }
    }

    fn into_message(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(message) = self.message {
            parts.push(message);
        }
        parts.extend(self.extras);
        parts.join(" ")
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_subscriber::layer::SubscriberExt;

    /// 全局落点是单例，用例必须串行跑；否则 A 的 sink 会收到 B 的事件。
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        match LOCK.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        }
    }

    /// 在独立 subscriber 里跑一段（不污染进程级全局 subscriber）。
    fn with_layer<F: FnOnce()>(redact: bool, body: F) {
        let subscriber = tracing_subscriber::registry().with(CoreLogLayer::new(redact));
        tracing::subscriber::with_default(subscriber, body);
    }

    fn collect() -> (Arc<Mutex<Vec<CoreLogRecord>>>, CoreLogSink) {
        let seen: Arc<Mutex<Vec<CoreLogRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_twin = Arc::clone(&seen);
        let sink: CoreLogSink = Arc::new(move |record| {
            let mut guard = match sink_twin.lock() {
                Ok(guard) => guard,
                Err(err) => err.into_inner(),
            };
            guard.push(record);
        });
        (seen, sink)
    }

    fn drain(seen: &Arc<Mutex<Vec<CoreLogRecord>>>) -> Vec<CoreLogRecord> {
        let mut guard = match seen.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        std::mem::take(&mut *guard)
    }

    #[test]
    fn forwards_tracing_events_as_core_log_records() {
        let _serial = serial();
        let (seen, sink) = collect();
        install_sink(sink);
        with_layer(true, || {
            // 显式 target：本模块自己的 target 被列入「不上报」，用它写用例等于自测假绿。
            tracing::warn!(target: "ai_ide_core::dispatch", plugin_id = "demo", "握手超时");
        });
        clear_sink();

        let records = drain(&seen);
        assert_eq!(records.len(), 1, "一条事件应上报一条：{records:?}");
        assert_eq!(records[0].level, "warn");
        assert_eq!(records[0].target, "ai_ide_core::dispatch");
        assert!(records[0].message.contains("握手超时"), "{records:?}");
        assert!(records[0].message.contains("plugin_id=demo"), "{records:?}");
    }

    #[test]
    fn no_sink_means_no_work() {
        let _serial = serial();
        assert!(!has_sink());
        // 未装落点时打日志不该 panic，也不该留下任何残留。
        with_layer(true, || tracing::info!("没人接的时候只是安静地过去"));
        assert!(!has_sink());
    }

    /// 最容易写出反馈回路的用法：sink 自己打日志。闸门必须把重入那条挡住。
    #[test]
    fn forwarding_does_not_recurse() {
        let _serial = serial();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        install_sink(Arc::new(move |record: CoreLogRecord| {
            counter.fetch_add(1, Ordering::SeqCst);
            // 模拟「上报路径上自己出了状况」：没有闸门就会无限重入。
            tracing::error!(target: "ai_ide_core::dispatch", "上报 {} 失败", record.target);
        }));
        with_layer(true, || {
            tracing::info!(target: "ai_ide_core::dispatch", "正常一条");
        });
        clear_sink();

        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "重入事件必须被闸门丢弃，否则会反馈回路"
        );
    }

    /// 自家模块与 `peer` 的日志不进通道：前者防自激，后者防「写失败→再写」放大。
    #[test]
    fn internal_targets_are_never_reported() {
        let _serial = serial();
        let (seen, sink) = collect();
        install_sink(sink);
        with_layer(true, || {
            tracing::error!(target: "ai_ide_core::core_log", "上报通道自己的一句话");
            tracing::error!(target: "ai_ide_core::core_log::pump", "pump 的一句话");
            tracing::error!(target: "ai_ide_core::peer", "写 stdout 失败");
            tracing::info!(target: "ai_ide_core::dispatch", "这条该上报");
        });
        clear_sink();

        let records = drain(&seen);
        assert_eq!(records.len(), 1, "只有 dispatch 那条该过：{records:?}");
        assert_eq!(records[0].target, "ai_ide_core::dispatch");
    }

    #[test]
    fn secrets_are_redacted_before_push() {
        let _serial = serial();
        let (seen, sink) = collect();
        install_sink(sink);
        let secret = "sk-live0123456789abcdefghijklmn";
        with_layer(true, || {
            tracing::info!(target: "ai_ide_core::dispatch", api_key = secret, "准备请求：key={secret}");
        });
        clear_sink();

        let records = drain(&seen);
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(
            !records[0].message.contains(secret),
            "明文密钥不该进事件：{}",
            records[0].message
        );
        assert!(
            records[0].message.contains("***"),
            "应留下打码痕迹：{}",
            records[0].message
        );
    }

    #[test]
    fn redaction_can_be_switched_off_for_local_debugging() {
        let _serial = serial();
        let (seen, sink) = collect();
        install_sink(sink);
        let secret = "sk-live0123456789abcdefghijklmn";
        with_layer(
            false,
            || tracing::info!(target: "ai_ide_core::dispatch", api_key = secret, "关闭脱敏"),
        );
        clear_sink();

        let records = drain(&seen);
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(records[0].message.contains(secret), "{records:?}");
    }

    #[test]
    fn long_messages_are_truncated_by_chars() {
        let long = "汉".repeat(MAX_MESSAGE_CHARS + 10);
        let record = CoreLogRecord {
            level: "info".to_string(),
            target: "ai_ide_core::dispatch".to_string(),
            message: truncate(long, MAX_MESSAGE_CHARS),
        };
        assert_eq!(record.message.chars().count(), MAX_MESSAGE_CHARS + 1);
        assert!(record.message.ends_with('…'));
    }

    #[test]
    fn levels_are_lowercase_for_the_shell_formatter() {
        assert_eq!(level_name(&tracing::Level::ERROR), "error");
        assert_eq!(level_name(&tracing::Level::WARN), "warn");
        assert_eq!(level_name(&tracing::Level::INFO), "info");
        assert_eq!(level_name(&tracing::Level::DEBUG), "debug");
        assert_eq!(level_name(&tracing::Level::TRACE), "trace");
    }

    /// `install_channel` 是 `run()` 用的那条路：装好之后事件应该能同步收到。
    #[test]
    fn channel_sink_delivers_end_to_end() {
        let _serial = serial();
        let mut receiver = install_channel();
        with_layer(
            true,
            || tracing::info!(target: "ai_ide_core::dispatch", "通道另一头收到了吗"),
        );

        // 先取再摘：`clear_sink` 会丢掉发送端，之后 `try_recv` 只会得到 Disconnected。
        let record = receiver.try_recv().expect("事件应已进入通道");
        clear_sink();
        assert_eq!(record.level, "info");
        assert!(record.message.contains("通道另一头收到了吗"));
    }
}
