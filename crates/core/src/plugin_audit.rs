//! 插件审计落行（P4 切片四）：把「能力变更」与「工具调用」统一成一种行形状。
//!
//! `plugin.host_call`（P3.5）只覆盖插件**向宿主**发起的调用；本模块补上另外三类
//! 由内核自己裁决的事件：
//!
//! | kind | 谁写的 | 回答什么问题 |
//! |---|---|---|
//! | [`audit_kind::PLUGIN_CAPABILITY_CHANGED`] | `plugin.grant` / `revoke` | 谁在什么时候把这个插件的哪项能力开了 / 关了 |
//! | [`audit_kind::PLUGIN_CALL`] | `plugin.call` 网关 | 模型 / 外壳试过调哪个插件的哪个工具，放行了还是被拒 |
//! | [`audit_kind::PLUGIN_LIFECYCLE`] | `plugin.load` / `unload` / `reload` | 插件的进程什么时候起、什么时候停 |
//!
//! 三种事件的明细字段是**同一套**（见 [`BASE_FIELDS`]）：`pluginId` / `action` /
//! `target` / `decision` / `callId`。审计面板只写一次渲染逻辑就能覆盖全部插件事件，
//! 排查时也不必记「这个 kind 里的目标叫 target 还是叫 path」。
//!
//! ## 为什么不用 `async fn` 一把梭
//!
//! [`record`] 是 `async` 的：`grant` / `revoke` / `call` 这些处理器本来就在
//! `await` 库，多 `await` 一次不多一层抽象；而 `host/*` 那条路径在插件帧泵里，
//! 只能 `spawn`（见 `plugin_host::FsHostApi::audit`），不能照抄。
//!
//! ## 插件改不了审计
//!
//! 这里没有任何「按插件 id 写审计」的入口：插件能碰到的只有 `HostApi::call`
//! 那六个 `host/*` 方法，`audit.*` 一个都不在其中（`host/audit.list` 会当场拿
//! `METHOD_NOT_FOUND`）。而仓储层 `audit` 表只有 `INSERT` 与 `SELECT`，
//! 连 `UPDATE` / `DELETE` 的 SQL 都不存在。

use ai_ide_store::{audit_actor, NewAuditEvent, Store};
use serde_json::{json, Value};

use crate::audit_api;

/// `action` 取值：与协议方法名同名，但去掉 `plugin.` 前缀。
pub const ACTION_GRANT: &str = "grant";
pub const ACTION_REVOKE: &str = "revoke";
pub const ACTION_CALL: &str = "call";
/// 用户触发插件命令（P5 切片二）。与 [`ACTION_CALL`] 分开：一个是模型调工具，
/// 一个是用户按了命令面板，出问题时该被怀疑的对象完全不同。
pub const ACTION_COMMAND: &str = "command";
pub const ACTION_LOAD: &str = "load";
pub const ACTION_UNLOAD: &str = "unload";
pub const ACTION_RELOAD: &str = "reload";

/// 明细里恒定出现的五个字段。写成常量是为了让查询侧 / 前端不必各自抄一遍。
pub const BASE_FIELDS: [&str; 5] = ["pluginId", "action", "target", "decision", "callId"];

/// 一行插件审计的原料。
///
/// 构造用 [`Self::new`]，再按需补 `target` / `call_id` / `note`——不用位置参数是
/// 因为这里过半字段都是 `Option`，排成一列没人分得清第几个 `None` 是谁。
#[derive(Debug, Clone)]
pub struct PluginAuditRow<'a> {
    kind: &'a str,
    plugin_id: &'a str,
    action: &'a str,
    decision: &'a str,
    target: Option<&'a str>,
    call_id: Option<&'a str>,
    extra: Value,
}

impl<'a> PluginAuditRow<'a> {
    pub fn new(kind: &'a str, plugin_id: &'a str, action: &'a str, decision: &'a str) -> Self {
        Self {
            kind,
            plugin_id,
            action,
            decision,
            target: None,
            call_id: None,
            extra: json!({}),
        }
    }

    /// 操作对象：能力名、工具名、安装目录……取决于 `action`。
    pub fn target(mut self, target: Option<&'a str>) -> Self {
        self.target = target;
        self
    }

    /// 调用标识。`plugin.call` 用请求 id（外壳生成，可跨日志对号）；
    /// 能力变更 / 生命周期不是「调用」，留空而不是编一个。
    pub fn call_id(mut self, call_id: Option<&'a str>) -> Self {
        self.call_id = call_id;
        self
    }

    /// 附一个额外字段（`capabilities` 列表、`unloaded` 之类）。值不是对象就塞进
    /// `value` 键，与 `NewAuditEvent::with_detail` 同一口径。
    pub fn note(mut self, key: &str, value: Value) -> Self {
        if let Value::Object(map) = &mut self.extra {
            map.insert(key.to_string(), value);
        }
        self
    }

    /// 拼成完整明细：五个基础字段在前，额外字段随后（同名不覆盖基础字段——
    /// 审计的固定列不能被调用点随手改掉）。
    fn detail(&self) -> Value {
        let mut detail = json!({
            "pluginId": self.plugin_id,
            "action": self.action,
            "target": self.target,
            "decision": self.decision,
            "callId": self.call_id,
        });
        if let (Value::Object(base), Value::Object(extra)) = (&mut detail, &self.extra) {
            for (key, value) in extra {
                base.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        detail
    }
}

/// 落一行插件审计。
///
/// **不传播错误**：调用点全是「业务已经做完」的位置，审计写不进去只能告警——
/// 否则会出现「能力明明授予了，却因为审计表写失败回一个 5003」的假失败。
pub async fn record(store: &Store, row: PluginAuditRow<'_>) {
    let summary = format!("插件 {} {}：{}", row.plugin_id, row.action, row.decision);
    let event = NewAuditEvent::by(audit_actor::PLUGIN, row.kind, summary).with_detail(row.detail());
    audit_api::record(store, event).await;
}

/// 一行「以用户身份」的插件审计。
///
/// 能力授予 / 撤销与装载 / 卸载都是**用户显式动作**，`actor` 记 `user` 而不是
/// `plugin`——否则审计里会出现「插件自己给自己开了 fs.write」这种读起来像越权的
/// 行，而实际上是用户在面板上点的。
pub async fn record_user(store: &Store, row: PluginAuditRow<'_>) {
    let summary = format!("插件 {} {}：{}", row.plugin_id, row.action, row.decision);
    let event = NewAuditEvent::by(audit_actor::USER, row.kind, summary).with_detail(row.detail());
    audit_api::record(store, event).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_store::audit_kind;

    #[test]
    fn detail_keeps_all_base_fields_and_extras() {
        let row = PluginAuditRow::new(audit_kind::PLUGIN_CALL, "demo", ACTION_CALL, "allow")
            .target(Some("fs.read"))
            .call_id(Some("req-1"))
            .note("truncated", json!(false));

        let detail = row.detail();
        for key in BASE_FIELDS {
            assert!(detail.get(key).is_some(), "缺字段 {key}");
        }
        assert_eq!(detail["pluginId"], "demo");
        assert_eq!(detail["target"], "fs.read");
        assert_eq!(detail["callId"], "req-1");
        assert_eq!(detail["truncated"], false);
    }

    #[test]
    fn extras_cannot_overwrite_base_fields() {
        let row = PluginAuditRow::new(audit_kind::PLUGIN_LIFECYCLE, "demo", ACTION_LOAD, "deny")
            .note("decision", json!("allow"));

        assert_eq!(row.detail()["decision"], "deny");
    }
}
