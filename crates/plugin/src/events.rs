//! 事件订阅（P5 切片一，docs/14 §6.5）。
//!
//! 事件是**宿主 → 插件单向**的通知帧（`plugin/event`），过滤靠清单里的
//! `subscriptions`：没声明的插件连队都不进，更不会因为一条事件被唤醒进程。
//!
//! 三件事在这一层定死，都在 host 之上、与进程无关，便于单测：
//!
//! 1. **订阅匹配**——精确名，或 `ns.*` 这类通配；空清单 = 什么都不订阅；
//! 2. **载荷最小化**——`workspace.changed` 只带路径（`{files:[...]}`），
//!    绝不带文件内容：事件是「有事发生了」的提示，不是数据通道，插件要内容
//!    得自己走 `host/fs.read`（那条路有闸门、有审计、有配额）；
//! 3. **高频源合并 + 每插件队列上限**——`workspace.changed` 的文件列表在队内
//!    合并去重，其余事件同类型只保留最新一份（后到覆盖先到，等于对突发抖动
//!    做去抖），队列满了丢最旧的一条并计数，绝不无限涨内存。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};

/// 宿主 → 插件的事件投递方法名（docs/14 §6.4）。
pub const EVENT_METHOD: &str = "plugin/event";

/// 工作区文件被改动（外部程序或内核写盘）。载荷 `{files:[...]}`。
pub const WORKSPACE_CHANGED: &str = "workspace.changed";
/// 单个文件被保存。载荷 `{path}`。
pub const FILE_SAVED: &str = "file.saved";
/// 会话开始。载荷 `{sessionId}`。
pub const SESSION_STARTED: &str = "session.started";
/// 会话结束。载荷 `{sessionId}`。
pub const SESSION_ENDED: &str = "session.ended";
/// 插件状态迁移。载荷 `{pluginId,state}`，**只有宿主能发**。
pub const PLUGIN_STATE_CHANGED: &str = "plugin.state_changed";
/// 插件日志（由 `host/log` 转成）。载荷 `{pluginId,level,message}`。
pub const PLUGIN_LOG: &str = "plugin.log";
/// 插件长任务进度（由 `host/progress` 转成）。
/// 载荷 `{pluginId,token,message,percent,truncated}`，见 [`progress_payload`]。
pub const PLUGIN_PROGRESS: &str = "plugin.progress";
/// 插件视图需要重拉（由 `host/view.refresh` 转成，docs/24 §4.5）。
///
/// 载荷 `{pluginId,viewId,rev}`，见 [`view_refresh_payload`]。这是**纯提示**：
/// 事件本身不带任何视图数据，收件方拿着 `viewId` 自己走 `plugin.call` 去取，
/// 那条路照样过闸门、过审计。于是「插件想让我重画」这件事既拿不到额外信息面，
/// 也没法把事件队列当成搬运通道刷满。
pub const PLUGIN_VIEW: &str = "plugin.view";

/// `host/progress` 单条消息的字符上限。
///
/// 200 足够写「正在扫描 src/components，第 3/8 个包」，又短到让一条进度帧
/// 永远撑不成数据通道——进度是「有事在发生」的提示，不是往外搬东西的路。
pub const MAX_PROGRESS_MESSAGE_CHARS: usize = 200;
/// `host/progress` 的 `token` 字符上限：它只用来把同一次长任务的多次上报串起来。
pub const MAX_PROGRESS_TOKEN_CHARS: usize = 64;

/// 每个插件的事件队列上限。
///
/// 取值不是拍的：一次工具调用最多 200 次宿主调用，队列 64 条足够覆盖一个
/// 抖动周期；再多的意义只在于「把内存换成延迟」，而插件真处理不过来时，
/// 丢掉旧事件比让内核一路涨内存好——事件本来就是增量提示，不是账本。
pub const MAX_QUEUE: usize = 64;
/// 单条 `workspace.changed` / `file.saved` 合并后的文件数上限。
pub const MAX_FILES_PER_EVENT: usize = 512;

/// 订阅匹配：精确名，或 `ns.*` 通配（`*` 也可以匹配全部）。
///
/// 不做前缀模糊匹配——`workspace` 不该匹配到 `workspace.changed`，
/// 那样「少写一段就多订阅一堆」的意外会在用户毫不知情时发生。
pub fn subscribed(subscriptions: &[String], event: &str) -> bool {
    subscriptions.iter().any(|rule| {
        let rule = rule.trim();
        if rule == "*" {
            return true;
        }
        if rule == event {
            return true;
        }
        match rule.strip_suffix(".*") {
            Some(namespace) => {
                event.len() > namespace.len()
                    && event.starts_with(namespace)
                    && event.as_bytes().get(namespace.len()) == Some(&b'.')
            }
            None => false,
        }
    })
}

/// `workspace.changed` 的载荷：**只有路径**，去重且保序，超上限截断。
pub fn workspace_changed_payload<I, S>(files: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen: Vec<String> = Vec::new();
    for file in files {
        let file = file.as_ref();
        if file.is_empty() || seen.iter().any(|kept| kept == file) {
            continue;
        }
        if seen.len() == MAX_FILES_PER_EVENT {
            break;
        }
        seen.push(file.to_string());
    }
    json!({ "files": seen })
}

/// `file.saved` 的载荷：只有路径。
pub fn file_saved_payload(path: &str) -> Value {
    json!({ "path": path })
}

/// `session.started` / `session.ended` 的载荷：只有会话 id。
pub fn session_payload(session_id: &str) -> Value {
    json!({ "sessionId": session_id })
}

/// `plugin.state_changed` 的载荷。
///
/// `plugin_id` 由宿主填：插件发不出这个事件（它只能发 `host/log` / `host/progress`），
/// 所以「伪造别人挂掉」这条路在协议层就不存在。
pub fn state_changed_payload(plugin_id: &str, state: &str) -> Value {
    json!({ "pluginId": plugin_id, "state": state })
}

/// `plugin.view` 的载荷：`{pluginId,viewId,rev}`。
///
/// 三个键全是**标识**，没有一个能装内容：`pluginId` 由宿主按进程句柄覆盖
/// （插件自报的值到不了这里），`viewId` 是清单里声明的那个 id，`rev` 是插件
/// 自己的单调计数，只用来让渲染侧判「这一帧比手上的新吗」。
///
/// 没有 `truncated` 标记：`viewId` 超长就是畸形参数（见 `host/view.refresh`
/// 的校验），静默截断反而会把它指到一个不存在的视图上——那比报错更难查。
pub fn view_refresh_payload(plugin_id: &str, view_id: &str, rev: u64) -> Value {
    json!({ "pluginId": plugin_id, "viewId": view_id, "rev": rev })
}

/// `plugin.progress` 的载荷：`{pluginId,token,message,percent,truncated}`。
///
/// 只认这三样 + 宿主盖的 `pluginId`：进度是**只读遥测**，不是往外搬文件内容
/// 的旁路，所以这里连一个透传 `extra` 的口子都不开（`host/log` 还有 `fields`，
/// 这里没有）。超上限按字符截断并置 `truncated: true`——与 `host/fs.read`
/// 的截断标记同一口径，收件方不必为「是不是被切了」另写一套判断。
pub fn progress_payload(
    plugin_id: &str,
    token: Option<&str>,
    message: Option<&str>,
    percent: Option<f64>,
) -> Value {
    let mut truncated = false;
    let token = token.map(|text| cap_chars(text, MAX_PROGRESS_TOKEN_CHARS, &mut truncated));
    let message = message.map(|text| cap_chars(text, MAX_PROGRESS_MESSAGE_CHARS, &mut truncated));
    // NaN / ±inf 不是百分比：`serde_json` 会把它们写成 `null`，不如自己先归零，
    // 免得「插件报了个 NaN」变成「这一帧载荷形状不一致」。
    let percent = percent
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0));
    json!({
        "pluginId": plugin_id,
        "token": token,
        "message": message,
        "percent": percent,
        "truncated": truncated,
    })
}

/// 按**字符**截到 `limit`（不是字节）：中文消息按字节切会切出半个字。
fn cap_chars(text: &str, limit: usize, truncated: &mut bool) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => {
            *truncated = true;
            text[..index].to_string()
        }
        None => text.to_string(),
    }
}

/// 入队结果。`Coalesced` / `Dropped` 不是错误，只是「这条没占新位置」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// 新增一条待发事件。
    Queued,
    /// 并进了队内同类型事件。
    Coalesced,
    /// 队列已满，丢掉了最旧的一条。
    Dropped,
}

/// 一条待发事件。
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedEvent {
    pub event: String,
    pub payload: Value,
}

/// 单插件的事件队列。
///
/// 合并规则按事件类型区分，不做「一刀切覆盖」：
///
/// - `workspace.changed`：文件列表**求并集**——丢文件等于让插件漏算索引；
/// - 其它事件（`file.saved` / `session.*` / `plugin.*`）：同类型只留最新，
///   后到的覆盖先到的。这类事件天然幂等，攒一堆「某文件被保存」没有意义。
pub struct EventQueue {
    cap: usize,
    pending: VecDeque<QueuedEvent>,
    /// 因队满被丢掉的条数（只增不减，供诊断与单测断言）。
    dropped: u64,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new(MAX_QUEUE)
    }
}

impl EventQueue {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            pending: VecDeque::new(),
            dropped: 0,
        }
    }

    /// 事件入队。
    pub fn push(&mut self, event: &str, payload: Value) -> PushOutcome {
        if let Some(existing) = self.pending.iter_mut().find(|queued| queued.event == event) {
            existing.payload = merge(event, &existing.payload, payload);
            return PushOutcome::Coalesced;
        }
        if self.pending.len() >= self.cap {
            self.pending.pop_front();
            self.dropped += 1;
            self.pending.push_back(QueuedEvent {
                event: event.to_string(),
                payload,
            });
            return PushOutcome::Dropped;
        }
        self.pending.push_back(QueuedEvent {
            event: event.to_string(),
            payload,
        });
        PushOutcome::Queued
    }

    pub fn pop(&mut self) -> Option<QueuedEvent> {
        self.pending.pop_front()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 队内快照（诊断/单测用，不消费）。
    pub fn snapshot(&self) -> Vec<QueuedEvent> {
        self.pending.iter().cloned().collect()
    }
}

/// 单插件的事件出口：一条带锁队列 + 一个「是否已有接线任务在跑」的旗标。
///
/// 一个插件同时只允许一个投递任务（`draining`）：事件按序写入 stdin，乱序
/// 会让插件看到「保存后又改动」这种时间倒流。任务把队列抽干就自己退出，
/// 下次 `push` 再拉起来——没有常驻线程，插件闲置时不占任何调度。
pub struct EventSink {
    plugin_id: String,
    queue: Mutex<EventQueue>,
    draining: AtomicBool,
}

impl EventSink {
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            queue: Mutex::new(EventQueue::default()),
            draining: AtomicBool::new(false),
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// 入队。队列不可用时（锁中毒）按丢弃处理，绝不 panic——事件丢一条比
    /// 拖垮整个内核好。
    pub fn push(&self, event: &str, payload: Value) -> PushOutcome {
        match self.queue.lock() {
            Ok(mut queue) => queue.push(event, payload),
            Err(_) => PushOutcome::Dropped,
        }
    }

    pub fn pop(&self) -> Option<QueuedEvent> {
        self.queue.lock().ok().and_then(|mut queue| queue.pop())
    }

    pub fn is_empty(&self) -> bool {
        self.queue
            .lock()
            .map(|queue| queue.is_empty())
            .unwrap_or(true)
    }

    pub fn len(&self) -> usize {
        self.queue.lock().map(|queue| queue.len()).unwrap_or(0)
    }

    pub fn dropped(&self) -> u64 {
        self.queue.lock().map(|queue| queue.dropped()).unwrap_or(0)
    }

    /// 尝试当「投递任务」：已经有任务在跑时返回 `false`。
    pub fn begin_drain(&self) -> bool {
        !self.draining.swap(true, Ordering::SeqCst)
    }

    /// 结束投递任务（无论正常收尾还是写失败）。
    pub fn end_drain(&self) {
        self.draining.store(false, Ordering::SeqCst);
    }
}

/// 合并两条同类型事件的载荷。
fn merge(event: &str, old: &Value, new: Value) -> Value {
    if event != WORKSPACE_CHANGED {
        return new;
    }
    let files = old
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            new.get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    workspace_changed_payload(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn subscriptions_match_exactly_or_by_namespace() {
        assert!(subscribed(
            &rules(&["workspace.changed"]),
            "workspace.changed"
        ));
        assert!(!subscribed(&rules(&["workspace.changed"]), "file.saved"));
        assert!(!subscribed(&rules(&["workspace"]), "workspace.changed"));
        assert!(!subscribed(&[], "workspace.changed"));
        // 通配：`session.*` 覆盖 session.started / session.ended，但不越界。
        assert!(subscribed(&rules(&["session.*"]), "session.started"));
        assert!(subscribed(&rules(&["session.*"]), "session.ended"));
        assert!(!subscribed(&rules(&["session.*"]), "sessionx.started"));
        assert!(!subscribed(&rules(&["session.*"]), "session"));
        assert!(subscribed(&rules(&["*"]), "whatever.happens"));
    }

    #[test]
    fn workspace_payload_carries_paths_only() {
        let payload = workspace_changed_payload(["src/a.ts", "src/b.ts", "src/a.ts"]);
        assert_eq!(payload, json!({ "files": ["src/a.ts", "src/b.ts"] }));
        // 载荷里不能出现内容字段——这是「事件不是数据通道」的硬约束。
        let object = payload.as_object().expect("对象");
        assert_eq!(object.len(), 1, "只允许 files 一个键");
        assert!(object.get("content").is_none());
        assert!(object.get("text").is_none());
    }

    #[test]
    fn workspace_payload_is_capped() {
        let files = (0..(MAX_FILES_PER_EVENT + 20)).map(|i| format!("f{i}.txt"));
        let payload = workspace_changed_payload(files);
        assert_eq!(
            payload["files"].as_array().map(Vec::len),
            Some(MAX_FILES_PER_EVENT)
        );
    }

    #[test]
    fn state_changed_payload_is_host_generated_with_plugin_id() {
        let payload = state_changed_payload("demo", "ready");
        assert_eq!(payload, json!({ "pluginId": "demo", "state": "ready" }));
    }

    /// 进度载荷只带白名单四样：插件塞进来的内容字段一律到不了事件里。
    #[test]
    fn progress_payload_carries_nothing_but_progress() {
        let payload = progress_payload("demo", Some("42"), Some("扫描 src"), Some(30.0));
        assert_eq!(
            payload,
            json!({
                "pluginId": "demo",
                "token": "42",
                "message": "扫描 src",
                "percent": 30.0,
                "truncated": false,
            })
        );
        let object = payload.as_object().expect("对象");
        assert_eq!(
            object.keys().collect::<Vec<_>>(),
            vec!["message", "percent", "pluginId", "token", "truncated"],
            "进度载荷的键是闭集"
        );
        assert!(object.get("content").is_none());
        assert!(object.get("files").is_none());
        assert!(object.get("path").is_none());
    }

    /// 超上限按字符截断并置 `truncated`；中文不能被切出半个字。
    #[test]
    fn progress_payload_truncates_and_marks() {
        let long_message = "进".repeat(MAX_PROGRESS_MESSAGE_CHARS + 40);
        let long_token = "t".repeat(MAX_PROGRESS_TOKEN_CHARS + 8);
        let payload = progress_payload("demo", Some(&long_token), Some(&long_message), Some(88.0));

        let message = payload["message"].as_str().expect("字符串");
        assert_eq!(message.chars().count(), MAX_PROGRESS_MESSAGE_CHARS);
        assert!(message.chars().all(|item| item == '进'), "要按字符切");
        assert_eq!(
            payload["token"].as_str().map(|text| text.chars().count()),
            Some(MAX_PROGRESS_TOKEN_CHARS)
        );
        assert_eq!(payload["truncated"], json!(true));
    }

    /// 百分比归一到 0–100；NaN / 缺省不伪装成数字。
    #[test]
    fn progress_percent_is_clamped_and_non_finite_dropped() {
        assert_eq!(
            progress_payload("demo", None, None, Some(130.0))["percent"],
            json!(100.0)
        );
        assert_eq!(
            progress_payload("demo", None, None, Some(-5.0))["percent"],
            json!(0.0)
        );
        assert_eq!(
            progress_payload("demo", None, None, Some(f64::NAN))["percent"],
            Value::Null,
            "NaN 不是进度"
        );
        assert_eq!(
            progress_payload("demo", None, None, None),
            json!({
                "pluginId": "demo",
                "token": null,
                "message": null,
                "percent": null,
                "truncated": false,
            })
        );
    }

    #[test]
    fn workspace_events_merge_file_lists_instead_of_dropping() {
        let mut queue = EventQueue::new(4);
        assert_eq!(
            queue.push(WORKSPACE_CHANGED, workspace_changed_payload(["a.ts"])),
            PushOutcome::Queued
        );
        assert_eq!(
            queue.push(
                WORKSPACE_CHANGED,
                workspace_changed_payload(["b.ts", "a.ts"])
            ),
            PushOutcome::Coalesced
        );
        assert_eq!(queue.len(), 1, "合并后仍只有一条");
        assert_eq!(
            queue.snapshot()[0].payload,
            json!({ "files": ["a.ts", "b.ts"] })
        );
    }

    #[test]
    fn repeated_events_keep_only_the_latest_payload() {
        let mut queue = EventQueue::new(4);
        queue.push(FILE_SAVED, file_saved_payload("a.ts"));
        queue.push(FILE_SAVED, file_saved_payload("b.ts"));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.snapshot()[0].payload, file_saved_payload("b.ts"));
        // 不同事件各占一条，不被对方吃掉。
        queue.push(SESSION_STARTED, session_payload("s1"));
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn queue_cap_drops_the_oldest_and_counts_it() {
        let mut queue = EventQueue::new(2);
        queue.push(SESSION_STARTED, session_payload("s1"));
        queue.push(SESSION_ENDED, session_payload("s2"));
        assert_eq!(
            queue.push(PLUGIN_LOG, json!({"pluginId":"demo"})),
            PushOutcome::Dropped
        );
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped(), 1);
        let events: Vec<String> = queue
            .snapshot()
            .into_iter()
            .map(|item| item.event)
            .collect();
        assert_eq!(
            events,
            vec![SESSION_ENDED.to_string(), PLUGIN_LOG.to_string()]
        );
    }

    #[test]
    fn queue_drains_in_order() {
        let mut queue = EventQueue::default();
        queue.push(WORKSPACE_CHANGED, workspace_changed_payload(["a.ts"]));
        queue.push(SESSION_STARTED, session_payload("s1"));
        let first = queue.pop().expect("第一条");
        assert_eq!(first.event, WORKSPACE_CHANGED);
        assert_eq!(queue.pop().expect("第二条").event, SESSION_STARTED);
        assert!(queue.pop().is_none());
        assert!(queue.is_empty());
    }
}
