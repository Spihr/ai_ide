//! 插件工具端到端（插件系统 P3：工具注入 + 审批映射）。
//!
//! 与 `plugin_flow.rs` 不同，这组用例不经过内核进程，而是直接把「真插件子进程 +
//! `plugin_tools::inject` + `PermissionGate` + 脚本化模型」拼成一次完整闭环：
//! 装载表 → 注册表 → 闸门 → 插件回包，链条上任何一环脱节都跑不通。
//!
//! 要钉住的是 docs/14 §13 给 P3 定的三条判定：
//!
//! 1. **模型可调用 `plugin.*` 工具**——注册表里的名字是 `plugin__<id>__<tool>`，
//!    调用真的落到插件进程，回包真的以工具结果回到模型面前；
//! 2. **`risk=write` 触发审批**——只读工具免问，写工具必须问；
//! 3. **拒绝后无副作用**——审批被拒时插件那边一次都没被调用过（用例里插件一旦
//!    被调用就会落一个标记文件，标记文件不存在才算过）。

use ai_ide_agent::message;
use ai_ide_agent::registry::ToolContext;
use ai_ide_agent::runner::{AgentLoop, ModelTurn, StopReason, TurnReply, TurnRequest};
use ai_ide_agent::{
    AgentError, ApprovalReply, ApprovalRequest, Decider, PermissionGate, ToolRegistry,
};
use ai_ide_plugin::host::{Host, LoadSpec};
use ai_ide_plugin::host_api::HostApi;
use ai_ide_plugin::install::inspect;
use ai_ide_protocol::dto::{ApprovalDecision, ChatToolCall};
use ai_ide_store::{audit_actor, audit_kind, AuditQuery, Store};
use ai_ide_workspace::WorkspaceManager;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 插件替身：`peek` 只读、`poke` 声明会写盘——**被调用才写标记文件**，
/// 于是「标记文件在不在」就等于「插件到底有没有被执行过」。
///
/// `probe` 是 P3.5 加的第三个工具：它在工具内部**反向请求宿主** `host/fs.read`，
/// 把拿到的工作区文件内容塞进工具回包。只有「插件进程 → 内核 → 宿主能力 →
/// 插件进程 → 工具结果 → 模型」这条环路真的通了，它的回包里才会有文件内容。
const PLUGIN_SCRIPT: &str = r#"
const fs = require('node:fs')
const path = require('node:path')
const readline = require('node:readline')

const marker = path.join(__dirname, 'poked.txt')
const tools = [
  { name: 'peek', description: '看一眼（只读）', risk: 'read',
    parameters: { type: 'object', properties: { q: { type: 'string' } } } },
  { name: 'poke', description: '动一下（会写盘）', risk: 'write',
    parameters: { type: 'object', properties: {} } },
  { name: 'probe', description: '读一个工作区文件（内部走 host/fs.read）', risk: 'read',
    parameters: { type: 'object', properties: { path: { type: 'string' } } } }
]

let seq = 1000
const pending = new Map()

function send(frame) {
  process.stdout.write(JSON.stringify(frame) + '\n')
}

// 反向请求宿主：帧 id 用数字，和宿主自己的请求 id 不撞（那条线是写死的 1/2/3）。
function askHost(method, params) {
  return new Promise((resolve) => {
    const id = ++seq
    pending.set(id, resolve)
    send({ jsonrpc: '2.0', id, method, params })
  })
}

async function handle(line) {
  const frame = JSON.parse(line)

  // 宿主对我们反向请求的回包：有 id、没有 method。
  if (frame.id !== undefined && frame.method === undefined && pending.has(frame.id)) {
    const done = pending.get(frame.id)
    pending.delete(frame.id)
    done(frame)
    return
  }

  const params = frame.params || {}
  if (frame.method === 'plugin/handshake') {
    send({ jsonrpc: '2.0', id: frame.id,
           result: { pluginApi: '1.0', name: 'demo', version: '0.1.0',
                     capabilities: ['fs.write'], tools } })
    return
  }
  if (frame.method === 'plugin/tool-invoke') {
    if (params.tool === 'poke') fs.writeFileSync(marker, 'poked\n')
    if (params.tool === 'probe') {
      const args = params.arguments || {}
      const reply = await askHost('host/fs.read', { path: args.path || 'note.txt' })
      const host = reply.error
        ? { error: reply.error }
        : { content: reply.result && reply.result.content,
            bytes: reply.result && reply.result.bytes,
            truncated: reply.result && reply.result.truncated }
      send({ jsonrpc: '2.0', id: frame.id, result: { tool: params.tool, host } })
      return
    }
    send({ jsonrpc: '2.0', id: frame.id,
           result: { tool: params.tool, arguments: params.arguments,
                     markerWritten: fs.existsSync(marker) } })
    return
  }
  send({ jsonrpc: '2.0', id: frame.id, result: { ok: true } })
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
rl.on('line', (line) => {
  if (!line.trim()) return
  handle(line).catch((err) => {
    send({ jsonrpc: '2.0', id: null,
           error: { code: -32603, message: String(err && err.message || err) } })
  })
})
"#;

const MANIFEST: &str = r#"{
  "id": "demo",
  "name": "演示插件",
  "version": "0.1.0",
  "pluginApi": "1.0",
  "main": "index.cjs",
  "capabilities": ["fs.write"],
  "tools": [
    { "name": "peek", "description": "看一眼（只读）", "risk": "read",
      "parameters": { "type": "object", "properties": { "q": { "type": "string" } } } },
    { "name": "poke", "description": "动一下（会写盘）", "risk": "write",
      "parameters": { "type": "object", "properties": {} } },
    { "name": "probe", "description": "读一个工作区文件（内部走 host/fs.read）", "risk": "read",
      "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } }
  ],
  "commands": [],
  "subscriptions": [],
  "timeoutMs": 30000
}"#;

/// 临时目录（自带清理）。工作区与插件包放在同一个根下，便于断言标记文件。
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|gap| gap.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "aiide-plugin-tool-e2e-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("建临时目录");
        Self { root }
    }

    fn plugin_dir(&self) -> PathBuf {
        let dir = self.root.join("plugin");
        std::fs::create_dir_all(&dir).expect("建插件目录");
        dir
    }

    /// 插件被真正执行过的证据文件。
    fn marker(&self) -> PathBuf {
        self.plugin_dir().join("poked.txt")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn node_ready() -> bool {
    std::process::Command::new(
        std::env::var(ai_ide_plugin::loader::NODE_ENV).unwrap_or_else(|_| "node".to_string()),
    )
    .arg("--version")
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|status| status.success())
    .unwrap_or(false)
}

/// 装一份最小插件并装载它。
async fn load_host(dir: &Path) -> Arc<Host> {
    load_host_with_api(dir, None).await
}

/// 同上，但装载前先把宿主能力实现塞进 `Host`（P3.5 才有这个入参）。
async fn load_host_with_api(dir: &Path, api: Option<Arc<dyn HostApi>>) -> Arc<Host> {
    std::fs::write(dir.join("plugin.json"), MANIFEST).expect("写清单");
    std::fs::write(dir.join("index.cjs"), PLUGIN_SCRIPT).expect("写脚本");
    let validated = inspect(dir).expect("清单应合法");
    let host = Arc::new(Host::new());
    if let Some(api) = api {
        host.set_api(api);
    }
    host.load(LoadSpec {
        plugin_id: "demo",
        install_dir: dir,
        validated: &validated,
        requested: vec!["fs.write".to_string()],
        granted: vec!["fs.write".to_string()],
        workspace: Value::Null,
    })
    .await
    .expect("装载应成功");
    host
}

/// 脚本化模型：按顺序吐预设回复，并记下每轮收到的请求。
struct Scripted {
    turns: Mutex<VecDeque<TurnReply>>,
    seen: Mutex<Vec<TurnRequest>>,
}

impl Scripted {
    fn new(turns: Vec<TurnReply>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.seen.lock().expect("seen 锁").len()
    }
}

#[async_trait]
impl ModelTurn for Scripted {
    async fn complete(&self, request: TurnRequest) -> Result<TurnReply, AgentError> {
        let reply = {
            self.seen.lock().expect("seen 锁").push(request);
            let mut turns = self.turns.lock().expect("turns 锁");
            turns
                .pop_front()
                .unwrap_or_else(|| TurnReply::text_only("（脚本已耗尽）"))
        };
        Ok(reply)
    }
}

fn call(id: &str, name: &str, args: Value) -> ChatToolCall {
    ChatToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

/// 假裁决者：记下问了什么，按预设动作回。
#[derive(Clone)]
struct StubDecider {
    calls: Arc<AtomicU64>,
    asked: Arc<Mutex<Vec<String>>>,
    decision: ApprovalDecision,
}

impl StubDecider {
    fn answering(decision: ApprovalDecision) -> Self {
        Self {
            calls: Arc::new(AtomicU64::new(0)),
            asked: Arc::new(Mutex::new(Vec::new())),
            decision,
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked 锁").clone()
    }
}

#[async_trait]
impl Decider for StubDecider {
    async fn decide(&self, request: &ApprovalRequest) -> Option<ApprovalReply> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.asked
            .lock()
            .expect("asked 锁")
            .push(request.tool.clone());
        Some(ApprovalReply::new(self.decision))
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 只读工具免问、写工具必须问，被拒后插件一次都没被调用。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_tool_runs_quietly_and_denied_write_never_reaches_the_plugin() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let sandbox = Sandbox::new("deny");
    let host = load_host(&sandbox.plugin_dir()).await;

    let registry = ai_ide_core::plugin_tools::inject(ToolRegistry::core(), Some(&host));
    assert!(
        registry.get("plugin__demo__peek").is_some(),
        "注册表里必须有命名空间化的插件工具"
    );

    let stub = StubDecider::answering(ApprovalDecision::Deny);
    let gate = PermissionGate::with_decider(stub.clone())
        .with_extra_read_tools(ai_ide_core::plugin_tools::read_tool_names(&host));

    let model = Scripted::new(vec![
        TurnReply::with_tool_calls(
            "先看一眼",
            vec![call("c1", "plugin__demo__peek", json!({"q": "你好"}))],
        ),
        TurnReply::with_tool_calls(
            "再动一下",
            vec![call("c2", "plugin__demo__poke", json!({}))],
        ),
        TurnReply::text_only("好，我不动了"),
    ]);

    let workspaces = WorkspaceManager::new();
    let workspace = workspaces
        .open(&sandbox.root.display().to_string())
        .expect("临时目录应能作为工作区");
    let ctx = ToolContext::new(&workspace);

    let outcome = AgentLoop::new(&model, &registry)
        .with_gate(&gate)
        .run(&ctx, vec![message::user_message("看一眼，再动一下")])
        .await
        .expect("拒绝不是致命错误，循环应继续");

    assert_eq!(outcome.stop_reason, StopReason::Completed);
    // 审批被拒的调用不算一步（工具没被执行），这是既有口径；两个调用各有一条结果。
    assert_eq!(outcome.steps, 1, "只有只读那一步真的执行了");

    let results: Vec<String> = outcome.tool_messages().map(|m| m.content.clone()).collect();
    assert_eq!(results.len(), 2, "每个调用都要有配对的结果");
    assert_eq!(
        outcome.assistant_errors.len(),
        1,
        "被拒的那次应记一条回灌错误"
    );
    assert!(results[0].contains("\"tool\":\"peek\""), "{}", results[0]);
    assert!(
        results[0].contains("你好"),
        "回包参数应原样回到模型：{}",
        results[0]
    );
    assert!(
        !results[0].contains("<tool_error"),
        "只读工具不该被拦：{}",
        results[0]
    );
    assert!(
        results[1].contains("<tool_error"),
        "写工具被拒应以工具错误回灌：{}",
        results[1]
    );

    assert_eq!(stub.calls(), 1, "只读免问，写工具问一次且只问一次");
    assert_eq!(stub.asked(), vec!["plugin__demo__poke".to_string()]);
    assert!(
        !sandbox.marker().exists(),
        "审批被拒后插件不该被执行：标记文件不存在才算无副作用"
    );

    host.shutdown_all().await;
}

/// 审批通过后写工具真的落到插件里（证明闸门放行的不是空壳）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approved_write_tool_executes_inside_the_plugin() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let sandbox = Sandbox::new("allow");
    let host = load_host(&sandbox.plugin_dir()).await;

    let registry = ai_ide_core::plugin_tools::inject(ToolRegistry::core(), Some(&host));
    let stub = StubDecider::answering(ApprovalDecision::AllowOnce);
    let gate = PermissionGate::with_decider(stub.clone())
        .with_extra_read_tools(ai_ide_core::plugin_tools::read_tool_names(&host));

    let model = Scripted::new(vec![
        TurnReply::with_tool_calls("动一下", vec![call("c1", "plugin__demo__poke", json!({}))]),
        TurnReply::text_only("动完了"),
    ]);

    let workspaces = WorkspaceManager::new();
    let workspace = workspaces
        .open(&sandbox.root.display().to_string())
        .expect("临时目录应能作为工作区");
    let ctx = ToolContext::new(&workspace);

    let outcome = AgentLoop::new(&model, &registry)
        .with_gate(&gate)
        .run(&ctx, vec![message::user_message("动一下")])
        .await
        .expect("审批通过后应跑完");

    assert_eq!(outcome.stop_reason, StopReason::Completed);
    assert_eq!(stub.calls(), 1);
    let result = outcome.tool_messages().next().expect("应有工具结果");
    assert!(
        result.content.contains("\"markerWritten\":true"),
        "插件的写动作应真被执行：{}",
        result.content
    );
    assert!(sandbox.marker().exists(), "标记文件应已落盘");
    assert_eq!(model.calls(), 2, "工具结果必须回到模型");

    host.shutdown_all().await;
}

/// P3.5：插件工具**内部**反向请求 `host/fs.read`。
///
/// 这条用例钉的是四段接线同时成立：插件帧泵把 `host/*` 交给宿主能力实现、
/// `FsHostApi` 通过能力闸门读到工作区文件、内容原样回到插件再回到模型、
/// 以及这次宿主调用在库里留下一条 `plugin.host_call` 审计。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_tool_reads_workspace_through_host_capability() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let sandbox = Sandbox::new("host");
    std::fs::write(sandbox.root.join("note.txt"), "HOST-READ-OK\n第二行\n").expect("写工作区文件");

    let store = Store::connect(&sandbox.root.join("audit.db"))
        .await
        .expect("打开审计库");
    let api = Arc::new(ai_ide_core::plugin_host::FsHostApi::new());
    api.set_root(Some(sandbox.root.clone()));
    api.set_granted_for("demo", vec!["fs.read".to_string()]);
    api.set_store(store.clone());

    let host = load_host_with_api(&sandbox.plugin_dir(), Some(api)).await;
    let registry = ai_ide_core::plugin_tools::inject(ToolRegistry::core(), Some(&host));
    assert!(
        registry.get("plugin__demo__probe").is_some(),
        "probe 应被注入注册表"
    );

    let stub = StubDecider::answering(ApprovalDecision::AllowOnce);
    let gate = PermissionGate::with_decider(stub.clone())
        .with_extra_read_tools(ai_ide_core::plugin_tools::read_tool_names(&host));

    let model = Scripted::new(vec![
        TurnReply::with_tool_calls(
            "读一个文件",
            vec![call(
                "c1",
                "plugin__demo__probe",
                json!({"path": "note.txt"}),
            )],
        ),
        TurnReply::text_only("读到了"),
    ]);

    let workspaces = WorkspaceManager::new();
    let workspace = workspaces
        .open(&sandbox.root.display().to_string())
        .expect("临时目录应能作为工作区");
    let ctx = ToolContext::new(&workspace);

    let outcome = AgentLoop::new(&model, &registry)
        .with_gate(&gate)
        .run(&ctx, vec![message::user_message("读一下 note.txt")])
        .await
        .expect("宿主能力已授权，循环应跑完");

    assert_eq!(outcome.stop_reason, StopReason::Completed);
    assert_eq!(stub.calls(), 0, "risk=read 的插件工具免问");

    let result = outcome.tool_messages().next().expect("应有工具结果");
    assert!(
        !result.content.contains("<tool_error"),
        "不该被闸门拦：{}",
        result.content
    );
    assert!(
        result.content.contains("HOST-READ-OK"),
        "工作区文件内容必须经 host/fs.read 回到模型：{}",
        result.content
    );
    assert_eq!(model.calls(), 2, "工具结果必须回到模型");

    // 审计是异步 `spawn` 落库的，轮询等它，别用固定 sleep 赌时序。
    let mut rows = Vec::new();
    for _ in 0..200 {
        rows = store
            .list_audit(AuditQuery::new().kind(audit_kind::PLUGIN_HOST_CALL))
            .await
            .unwrap_or_default();
        if !rows.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(rows.len(), 1, "一次宿主调用应落一行 plugin.host_call 审计");
    let row = &rows[0];
    assert_eq!(row.actor, audit_actor::PLUGIN, "发起方应是 plugin");
    assert_eq!(row.detail["pluginId"], "demo");
    assert_eq!(row.detail["method"], "host/fs.read");
    assert_eq!(row.detail["capability"], "fs.read");
    assert_eq!(row.detail["target"], "note.txt");
    assert_eq!(row.detail["decision"], "allow");
    assert!(
        row.detail["callId"]
            .as_str()
            .unwrap_or("")
            .starts_with("host-"),
        "callId 应带上序号：{}",
        row.detail
    );

    host.shutdown_all().await;
}
