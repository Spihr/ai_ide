//! `permission.list` / `permission.revoke` 的端到端测试（M5 切片六）。
//!
//! 免问规则有个尴尬处：它**只能**由用户当场点「总是允许」产生，协议面没有授予入口
//! （能凭空授权的接口等于自我提权）。所以这里用两个视角：
//!
//! - 黑盒视角：起真实内核子进程，只用 stdin / stdout 问 `permission.*`；
//! - 数据准备：测试进程自己开同一个 `sessions.db` 写规则，模拟「上一次会话里用户
//!   点过总是允许」留下的状态。
//!
//! 两者用同一个库文件，正是要验证「外部写入的规则，内核进程能读到并撤销」——这也是
//! 用户重启 IDE 后真实发生的事。

mod common;

use ai_ide_store::{NewPermissionRule, Store};
use common::{unique_temp_dir, Core, TempDataDir};
use serde_json::json;
use std::path::{Path, PathBuf};

/// 工作区临时目录：`permission.list` 的 scope 过滤需要真实路径。
struct TempDir {
    root: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        Self {
            root: unique_temp_dir("ai-ide-m5-perm", tag),
        }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    /// 内核侧 `scope` 的口径：绝对路径 + 正斜杠。
    fn scope(&self) -> String {
        self.root.to_string_lossy().replace('\\', "/")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn db_path(data: &TempDataDir) -> PathBuf {
    data.root.join("sessions.db")
}

async fn seed_rule(data: &TempDataDir, id: &str, tool: &str, scope: &str) {
    let store = Store::connect(&db_path(data)).await.expect("打开会话库");
    store
        .remember_permission(NewPermissionRule::allow(id, tool, scope))
        .await
        .expect("写入规则");
}

#[test]
fn capabilities_advertise_permission_methods() {
    let data = TempDataDir::new("perm-caps");
    let mut core = Core::spawn(&data.root);

    let capabilities = core.capabilities();
    for method in ["permission.list", "permission.revoke"] {
        assert!(
            capabilities.iter().any(|name| name == method),
            "能力清单缺少 {method}：{capabilities:?}"
        );
    }
    // 授予必须留在 `agent.decide`，协议面不得出现 `permission.allow` 之类的入口。
    assert!(
        capabilities
            .iter()
            .filter(|name| name.starts_with("permission."))
            .count()
            == 2,
        "permission 域只应有 list / revoke：{capabilities:?}"
    );
}

#[test]
fn empty_ledger_lists_zero_rules() {
    let data = TempDataDir::new("perm-empty");
    let mut core = Core::spawn(&data.root);

    let listed = core.ok("permission.list", json!({}));
    assert_eq!(
        listed["total"].as_u64(),
        Some(0),
        "空库总数应为 0：{listed}"
    );
    assert!(
        listed["rules"].as_array().expect("rules").is_empty(),
        "空库不该有规则：{listed}"
    );
}

#[test]
fn lists_rules_written_from_another_connection() {
    let data = TempDataDir::new("perm-list");
    let workspace = TempDir::new("ws");
    let scope = workspace.scope();
    let mut core = Core::spawn(&data.root);

    // 同 `revoke_by_id_and_by_tool`：先握手等内核迁移落库，再让测试进程写种子规则，
    // 避免两个进程对同一个 `sessions.db` 并发跑迁移。
    core.capabilities();

    // 模拟「上一次会话里点过总是允许」：外部连接写进同一个库。
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            seed_rule(&data, "command:cargo", "run_command", &scope).await;
            seed_rule(&data, "tool:fetch", "fetch", "global").await;
            seed_rule(&data, "path:edit:/other", "edit", "/other").await;
        });

    let opened = core.ok("workspace.open", json!({ "path": workspace.path() }));
    let workspace_id = opened["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();

    let listed = core.ok("permission.list", json!({ "workspace_id": workspace_id }));
    let rules = listed["rules"].as_array().expect("rules");
    let ids: Vec<&str> = rules
        .iter()
        .map(|rule| rule["id"].as_str().expect("规则 id"))
        .collect();

    assert_eq!(
        listed["total"].as_u64(),
        Some(2),
        "本工作区 + global 共两条：{listed}"
    );
    assert!(
        ids.contains(&"command:cargo"),
        "本工作区规则要看得见：{ids:?}"
    );
    assert!(
        ids.contains(&"tool:fetch"),
        "global 规则在哪都看得见：{ids:?}"
    );
    assert!(
        !ids.contains(&"path:edit:/other"),
        "别的工作区规则不该混进来：{ids:?}"
    );

    let first = &rules[0];
    assert!(first["tool_pattern"].as_str().is_some(), "工具名应回给 UI");
    assert!(first["policy"].as_str().is_some(), "策略字段应回给 UI");
    assert!(first["created_at_ms"].as_i64().is_some(), "时间戳应回给 UI");
}

#[test]
fn revoke_by_id_and_by_tool() {
    let data = TempDataDir::new("perm-revoke");
    let workspace = TempDir::new("ws");
    let scope = workspace.scope();
    let mut core = Core::spawn(&data.root);

    // 先握手，等内核把启动流程走完（`Store::connect` 的迁移至此落库），再让测试进程
    // 写种子规则。否则两个进程会同时对着同一个 `sessions.db` 跑 `MIGRATOR.run`，
    // CI runner 冷启动慢时偶发抢锁超时，内核侧迁不上去就退化成「会话库未就绪 5002」
    // （docs/11 §5、docs/12 §4.5 记录的正是这个偶发）。
    core.capabilities();

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            seed_rule(&data, "path:edit:a", "edit", &scope).await;
            seed_rule(&data, "path:edit:b", "edit", &scope).await;
            seed_rule(&data, "command:cargo", "run_command", &scope).await;
        });

    let revoked = core.ok("permission.revoke", json!({ "id": "command:cargo" }));
    assert_eq!(
        revoked["removed"].as_u64(),
        Some(1),
        "按 id 应删掉一条：{revoked}"
    );
    assert_eq!(
        revoked["remaining"].as_u64(),
        Some(2),
        "剩余条数应回给 UI：{revoked}"
    );

    // 再撤一次同一条：不应报错，只是没删到东西。
    let again = core.ok("permission.revoke", json!({ "id": "command:cargo" }));
    assert_eq!(
        again["removed"].as_u64(),
        Some(0),
        "重复撤销应幂等：{again}"
    );

    let by_tool = core.ok("permission.revoke", json!({ "tool": "edit" }));
    assert_eq!(
        by_tool["removed"].as_u64(),
        Some(2),
        "按工具删掉两条：{by_tool}"
    );
    assert_eq!(
        by_tool["remaining"].as_u64(),
        Some(0),
        "撤干净了：{by_tool}"
    );

    let listed = core.ok("permission.list", json!({}));
    assert_eq!(
        listed["total"].as_u64(),
        Some(0),
        "撤完不该再有规则：{listed}"
    );
}

#[test]
fn revoke_without_target_is_rejected() {
    let data = TempDataDir::new("perm-params");
    let mut core = Core::spawn(&data.root);

    // 空对象 = 不知道撤哪条。整批清空不开放给协议调用，必须退回去。
    let response = core.call("permission.revoke", json!({}));
    let error = response.get("error").expect("应返回错误");
    assert_eq!(
        error["code"].as_i64(),
        Some(-32602),
        "应为 invalid params：{error}"
    );

    // 空白串同样等于「没给」。
    let blank = core.call("permission.revoke", json!({ "id": "   ", "tool": "" }));
    assert!(
        blank.get("error").is_some(),
        "全空白的目标也应按缺参拒绝：{blank}"
    );
}

#[test]
fn rules_survive_core_restart() {
    let data = TempDataDir::new("perm-restart");
    let workspace = TempDir::new("ws");
    let scope = workspace.scope();

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(seed_rule(&data, "command:cargo", "run_command", &scope));

    {
        let mut core = Core::spawn(&data.root);
        let listed = core.ok("permission.list", json!({}));
        assert_eq!(
            listed["total"].as_u64(),
            Some(1),
            "第一次启动应看到规则：{listed}"
        );
    }

    // 重启一个新内核：规则是落盘的，不该随进程消失。
    let mut core = Core::spawn(&data.root);
    let listed = core.ok("permission.list", json!({}));
    let rules = listed["rules"].as_array().expect("rules");
    assert_eq!(listed["total"].as_u64(), Some(1), "重启后应仍在：{listed}");
    assert_eq!(
        rules[0]["id"].as_str(),
        Some("command:cargo"),
        "键必须逐字还原（不能推导成更粗的粒度）"
    );
    assert_eq!(rules[0]["scope"].as_str(), Some(scope.as_str()));
}

#[test]
fn list_rejects_unknown_workspace() {
    let data = TempDataDir::new("perm-bad-ws");
    let mut core = Core::spawn(&data.root);

    // 显式指定一个没打开的工作区：不能悄悄退回「列全部」，否则 UI 会显示错的账。
    let response = core.call(
        "permission.list",
        json!({ "workspace_id": "ws-does-not-exist" }),
    );
    let error = response.get("error").expect("应返回错误");
    assert_eq!(
        error["code"].as_i64(),
        Some(1001),
        "应为 WORKSPACE_NOT_OPENED：{error}"
    );
}
