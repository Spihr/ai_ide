//! 插件写侧的生产审批链路（P4 切片三）。
//!
//! 切片二把四道闸门装进了 [`FsHostApi`](crate::plugin_host::FsHostApi)，但只留下
//! `WriteApprover` 这个「插座」：没有实现方，线上任何 `host/fs.write` 都会在第三关
//! 拿到 `3001`。本模块补上插头，并把它接回**已有一模一样的审批通道**——
//! [`PermissionGate`] + 裁决方 + `agent.approval_required` 事件 + `agent.decide`
//! 回执，与 agent 循环里 `run_command` / `edit` 走的是同一条路。
//!
//! ## 为什么不另起一套协议
//!
//! 外观侧只需实现一次审批 UI。若插件写盘另开事件名、另起 `approval_id`，
//! 前端就得维护第二套「待裁决」状态机，而两边的 `allow_always` 记忆、超时、
//! 拒绝话术迟早会走岔。复用后，用户看到的差异只有 `tool` 字段：`fs.write`。
//!
//! ## 同步签名与线程
//!
//! [`WriteApprover::decide`] 是同步的，因为它跑在插件 stdout 的帧泵线程上，
//! 审批却要等 UI。这里用「把异步等答复丢给 tokio，再用 `recv_timeout` 同步等」
//! 过桥：等不到（超时 / 没裁决者 / 没工作区）一律 [`None`]，也就是**拒**。
//!
//! ## 「总是允许」落盘
//!
//! 命中 `allow_always` 时写一条免问规则（`tool_pattern` = 文件绝对路径的
//! `scope_key`，与 [`FsHostApi`](crate::plugin_host::FsHostApi) 判重用的是同一把钥匙），
//! 打开工作区时再灌回去。落盘是 `tokio::spawn` 尽力而为：写库失败只会导致
//! 「下次再问」，不会变成静默放行。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ai_ide_agent::{Grant, PermissionGate, ToolCall};
use ai_ide_protocol::dto::ApprovalDecision;
use ai_ide_store::{NewPermissionRule, Store, MAX_PERMISSION_RULES};

use crate::permission_api::scope_for_root;
use crate::plugin_host::{write_scope_key, WriteApproval, WriteApprovalRequest, WriteApprover};

/// 写侧审批在 UI 里的工具名。
///
/// 刻意与宿主方法同名（`fs.write`）：面板上看到的就是插件实际调的那一关。
pub const WRITE_TOOL: &str = "fs.write";

/// 裁决方比闸门多等的余量。
///
/// 闸门自己已经带着超时去等 UI 了，外层只需比它稍宽一点——否则会出现
/// 「闸门刚拿到答复，这里已经放弃」，把一次明确的 `allow` 丢成 `None`。
const REPLY_SLACK: Duration = Duration::from_secs(5);

/// 把插件写侧的审批问到用户面前的裁决方。
///
/// `root` / `store` 与 [`FsHostApi`](crate::plugin_host::FsHostApi) 共享同一份
/// `Arc<RwLock<_>>`：审批要落在「当前活动工作区」名下，而工作区是随 `ws.open` /
/// `ws.close` 变的，拷一份快照进来会在切换工作区后写错作用域。
pub struct PluginWriteApprover {
    gate: Arc<PermissionGate>,
    root: Arc<RwLock<Option<PathBuf>>>,
    store: Arc<RwLock<Option<Store>>>,
    timeout: Duration,
}

impl PluginWriteApprover {
    /// `timeout` 是**同步等回执**的上限，调用方给「闸门超时 + 余量」。
    pub fn new(
        gate: PermissionGate,
        root: Arc<RwLock<Option<PathBuf>>>,
        store: Arc<RwLock<Option<Store>>>,
        timeout: Duration,
    ) -> Self {
        Self {
            gate: Arc::new(gate),
            root,
            store,
            timeout,
        }
    }

    /// 把 `allow_always` 落成一条免问规则（尽力而为，失败只影响「下次还要问」）。
    fn persist(&self, scope: &str, plugin_id: &str) {
        let store = self.store.read().ok().and_then(|slot| slot.clone());
        let Some(store) = store else {
            // 库没连上（启动早期 / 单测）：进程内仍然记住了，不清空用户这次的选择。
            tracing::debug!(scope = %scope, "会话库未就绪，写侧免问规则未落盘");
            return;
        };
        let root = self.root.read().ok().and_then(|slot| slot.clone());
        let scope_owner = root
            .as_deref()
            .map(|root| scope_for_root(&root.to_string_lossy()))
            .unwrap_or_else(|| "global".to_string());
        let key = scope.to_string();
        // 调用点没有 async 上下文可用（帧泵线程），也不能 `block_on`：把落盘丢出去。
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::debug!(scope = %scope, "无 tokio 运行时，写侧免问规则未落盘");
            return;
        };
        let plugin_id = plugin_id.to_string();
        let scope_owner = scope_owner.to_string();
        handle.spawn(async move {
            let rule = NewPermissionRule::allow(&key, WRITE_TOOL, &scope_owner);
            if let Err(err) = store.remember_permission(rule).await {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    scope = %key,
                    error = %err,
                    "写侧免问规则落盘失败（下次仍会询问）"
                );
            }
        });
    }
}

impl WriteApprover for PluginWriteApprover {
    fn decide(&self, request: &WriteApprovalRequest) -> Option<WriteApproval> {
        let Some(root) = self.root.read().ok().and_then(|slot| slot.clone()) else {
            // 走到写侧却没有活动工作区，说明上层闸门漏了；这里不表态（= 拒）。
            tracing::warn!(
                plugin_id = %request.plugin_id,
                path = %request.path,
                "写审批：没有活动工作区，按拒绝处理"
            );
            return None;
        };

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("写审批：无 tokio 运行时，无法等用户回执，按拒绝处理");
            return None;
        };

        let (tx, rx) = mpsc::channel();
        let gate = self.gate.clone();
        let path = request.path.clone();
        let reason = format!(
            "插件 {} 请求写入 {}（{}）",
            request.plugin_id, request.path, request.method
        );
        handle.spawn(async move {
            let call = ToolCall::new(WRITE_TOOL).path(&path);
            let verdict = gate.request_approval(Path::new(&root), &call, reason).await;
            // 接收端可能已超时走人：丢这个不算错误。
            let _ = tx.send(verdict);
        });

        match rx.recv_timeout(self.timeout) {
            Ok(Ok(Grant::Policy | Grant::Cached)) => Some(WriteApproval::AllowOnce),
            Ok(Ok(Grant::Approved(reply))) => match reply.decision {
                ApprovalDecision::AllowOnce => Some(WriteApproval::AllowOnce),
                ApprovalDecision::AllowAlways => {
                    // 闸门已经在自己那份内存里记过了；宿主这份也得记住，
                    // 否则同一文件的下一次写还要再过一遍审批。
                    self.persist(&request.scope_key, &request.plugin_id);
                    Some(WriteApproval::AllowAlways)
                }
                ApprovalDecision::Deny | ApprovalDecision::DenyWithReason => {
                    if let Some(reason) = reply.reason.as_deref() {
                        tracing::info!(
                            plugin_id = %request.plugin_id,
                            path = %request.path,
                            reason = %reason,
                            "写审批被拒绝"
                        );
                    }
                    Some(WriteApproval::Reject)
                }
            },
            Ok(Err(err)) => {
                tracing::info!(
                    plugin_id = %request.plugin_id,
                    path = %request.path,
                    error = %err,
                    "写审批未获批准（闸门判拒）"
                );
                None
            }
            Err(err) => {
                tracing::warn!(
                    plugin_id = %request.plugin_id,
                    path = %request.path,
                    error = %err,
                    "写审批等待回执超时，按拒绝处理"
                );
                None
            }
        }
    }
}

/// 装上（或换掉）生产裁决方。
///
/// 在 `ws.open` 里调：此时对端句柄与活动工作区都已就位。重复调用是替换语义：
/// 重连后审批会自动跟到新对端上——否则旧 `Peer` 被丢弃，插件写盘会一直等一个
/// 已经没人读的通道，直到超时判拒。
///
/// 没装对端（理论上只有启动早期 / 单测）就什么都不装：宁可写请求继续撞
/// `3001`，也不要挂一个永远问不出去的裁决方。
pub fn install(state: &crate::state::AppState) {
    let Some(peer) = state.peer() else {
        tracing::debug!("对端未就绪，插件写侧审批暂不安装");
        return;
    };
    let gate = PermissionGate::with_decider(crate::agent_api::PeerDecider::for_plugin_host(
        peer,
        state.agent(),
    ))
    .with_timeout(crate::agent_api::APPROVAL_TIMEOUT);
    let host = state.plugin_host();
    let approver = PluginWriteApprover::new(
        gate,
        host.root_handle(),
        host.store_handle(),
        crate::agent_api::APPROVAL_TIMEOUT + REPLY_SLACK,
    );
    host.set_write_approver(Arc::new(approver));
    tracing::debug!("插件写侧审批已接回 agent 审批通道");
}

/// 把库里给**这个工作区**的写侧免问规则灌回宿主，返回灌入条数。
///
/// 覆盖语义（[`FsHostApi::seed_write_always`](crate::plugin_host::FsHostApi::seed_write_always)）：
/// `ws.open` 传新工作区，`ws.close` 传下一个活动区（`None`）。撤销一条规则之后必须能真的
/// 忘掉，追加语义做不到这一点——「已关闭的工作区仍然免问」是极难解释的一种泄漏。
///
/// 读库失败 / 库没连上 → 灌 0 条：宁可多问一次，也不要静默放行。
pub async fn seed_write_always(state: &crate::state::AppState, root: Option<&Path>) -> usize {
    let api = state.plugin_host();
    let Some(root) = root else {
        api.seed_write_always(Vec::new());
        return 0;
    };
    let Some(store) = state.store().cloned() else {
        api.seed_write_always(Vec::new());
        return 0;
    };
    let scope = scope_for_root(&root.to_string_lossy());
    let rules = match store
        .list_permissions(Some(&scope), MAX_PERMISSION_RULES)
        .await
    {
        Ok(rules) => rules,
        Err(err) => {
            tracing::warn!(%scope, error = %err, "读写侧免问规则失败，按「一条都没有」处理");
            api.seed_write_always(Vec::new());
            return 0;
        }
    };
    // 只认写侧工具：同一张表里还躲着 `run_command` 的免问规则，混进来就等于
    // 把「批准过一次命令」翻译成「这个文件随便写」。
    let keys: Vec<String> = rules
        .into_iter()
        .filter(|rule| rule.tool_pattern == WRITE_TOOL)
        .map(|rule| rule.id)
        .collect();
    let seeded = keys.len();
    api.seed_write_always(keys);
    seeded
}

/// 规则键就是 `scope_key`（绝对路径），这里只做一次显式换算，便于调用方对齐。
pub fn rule_key(path: &Path) -> String {
    write_scope_key(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_agent::{ApprovalReply, Decider};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 只回一个固定动作的裁决者；顺带记下被问了几次。
    struct StubDecider {
        reply: Option<ApprovalReply>,
        calls: Arc<AtomicUsize>,
    }

    impl StubDecider {
        fn answering(decision: ApprovalDecision) -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    reply: Some(ApprovalReply::new(decision)),
                    calls: Arc::clone(&calls),
                },
                calls,
            )
        }

        fn silent() -> Self {
            Self {
                reply: None,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl Decider for StubDecider {
        async fn decide(&self, _request: &ai_ide_agent::ApprovalRequest) -> Option<ApprovalReply> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.reply.clone()
        }
    }

    fn make_approver(
        decider: StubDecider,
        root: Option<PathBuf>,
        store: Option<Store>,
    ) -> (PluginWriteApprover, Arc<RwLock<Option<Store>>>) {
        let store_slot = Arc::new(RwLock::new(store));
        let gate = PermissionGate::with_decider(decider).with_timeout(Duration::from_secs(1));
        let approver = PluginWriteApprover::new(
            gate,
            Arc::new(RwLock::new(root)),
            Arc::clone(&store_slot),
            Duration::from_secs(2),
        );
        (approver, store_slot)
    }

    fn request() -> WriteApprovalRequest {
        WriteApprovalRequest {
            plugin_id: "demo-plugin".to_string(),
            method: "host/fs.write".to_string(),
            path: "C:/ws/a.txt".to_string(),
            scope_key: "c:/ws/a.txt".to_string(),
        }
    }

    /// 批准一次 → 放行这一次；「总是允许」→ 同时把免问规则写进库。
    ///
    /// 必须用多线程运行时：`decide` 是同步阻塞的（`recv_timeout` 等回执），而它内部
    /// 把真正的审批 `spawn` 给 tokio 上。单线程运行时下，阻塞的那根线程正是唯一的
    /// 工作线程，spawn 出去的任务永远排不上号，只能等到超时判拒——线上不会这样
    /// （帧泵是独立 OS 线程），但测试里会撞上。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn allow_once_passes_and_allow_always_persists() {
        let (decider, calls) = StubDecider::answering(ApprovalDecision::AllowOnce);
        let store = Store::open_in_memory().await.expect("内存库应能打开");
        let (approver, _slot) =
            make_approver(decider, Some(PathBuf::from("C:/ws")), Some(store.clone()));

        assert_eq!(approver.decide(&request()), Some(WriteApproval::AllowOnce));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 换一个「总是允许」的裁决方：这一次要落库。
        let (decider, _calls) = StubDecider::answering(ApprovalDecision::AllowAlways);
        let (approver, _slot) =
            make_approver(decider, Some(PathBuf::from("C:/ws")), Some(store.clone()));
        assert_eq!(
            approver.decide(&request()),
            Some(WriteApproval::AllowAlways)
        );

        // 落盘是 spawn 出去的：等它跑完再看库。
        for _ in 0..50 {
            let rules = store
                .list_permissions(None, MAX_PERMISSION_RULES)
                .await
                .expect("读规则");
            if let Some(rule) = rules.iter().find(|rule| rule.id == request().scope_key) {
                assert_eq!(rule.tool_pattern, WRITE_TOOL, "只记写侧工具");
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("allow_always 的免问规则没有落盘");
    }

    /// 拒绝 / 没答复 / 没工作区：三种都必须变成「不写」。
    ///
    /// 三种在这里都收敛到 `None`，不是偷懒：[`PermissionGate`] 把「用户明确拒绝」
    /// 和「没通道 / 超时 / 没答复」一律翻成 `Err(TOOL_NOT_ALLOWED)`，回执里的
    /// `deny` 原文到此已经不带出来了，而宿主拿到 `None` 与 `Some(Reject)` 的处置
    /// 完全一样——`3001` + 一条 `reject` 审计，都写不出文件。真要在审计里区分
    /// 「谁拒的」得让闸门把原始 decision 带出来，那是闸门侧的改动，不该在这里
    /// 靠错误文案猜。
    ///
    /// 与 [`allow_once_passes_and_allow_always_persists`] 一样，需要多线程运行时。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_unclear_answer_means_no_write() {
        let (decider, calls) = StubDecider::answering(ApprovalDecision::Deny);
        let (approver, _slot) = make_approver(decider, Some(PathBuf::from("C:/ws")), None);
        assert_eq!(approver.decide(&request()), None, "用户拒绝 = 不写");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "拒绝也要真的问过一次");

        let (approver, _slot) =
            make_approver(StubDecider::silent(), Some(PathBuf::from("C:/ws")), None);
        assert_eq!(approver.decide(&request()), None, "没有答复 = 不表态 = 拒");

        let (decider, calls) = StubDecider::answering(ApprovalDecision::AllowOnce);
        let (approver, _slot) = make_approver(decider, None, None);
        assert_eq!(approver.decide(&request()), None, "没开工作区不能批");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "连问都不该问");
    }

    /// 灌回免问集合时只认写侧规则，别的工具的同作用域规则不能顺带放行写盘。
    #[tokio::test]
    async fn seeding_only_takes_write_rules_back() {
        let state = crate::state::AppState::new();
        let store = Store::open_in_memory().await.expect("内存库应能打开");
        state.install_store(store.clone());

        let root = if cfg!(windows) { "C:/ws" } else { "/ws" };
        let scope = scope_for_root(root);
        let key = write_scope_key(Path::new(&format!("{root}/a.txt")));
        store
            .remember_permission(NewPermissionRule::allow(&key, WRITE_TOOL, &scope))
            .await
            .expect("写规则");
        store
            .remember_permission(NewPermissionRule::allow(&scope, "run_command", &scope))
            .await
            .expect("他人规则");

        let seeded = seed_write_always(&state, Some(Path::new(root))).await;
        assert_eq!(seeded, 1);
        assert!(state.plugin_host().is_write_always(&key));
        assert!(
            !state.plugin_host().is_write_always(&scope),
            "命令类规则不能变成写盘免问"
        );

        // 关掉工作区：集合清空，旧根不再免问。
        assert_eq!(seed_write_always(&state, None).await, 0);
        assert!(!state.plugin_host().is_write_always(&key));
    }
}
