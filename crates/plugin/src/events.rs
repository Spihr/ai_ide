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
/// 插件长任务进度（由 `host/progress` 转成）。载荷 `{pluginId,progress}`。
pub const PLUGIN_PROGRESS: &str = "plugin.progress";

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
