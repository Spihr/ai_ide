//! `plugin.command` 端到端（插件系统 P5 切片二：命令网关）。
//!
//! 与 `plugin_tool_flow.rs` 的分工：那边把「装载表 + 注册表 + 闸门 + 插件回包」
//! 在进程内拼起来，这边走**真实内核子进程 + 真实 node 插件包**，因为命令这条线
//! 最容易出错的地方全在内核里——十道闸门的**顺序**、审批要经过 `agent` 通道兜一圈
//! 再回来、审计要落到 `plugin.command` 这个 kind 上。进程内自己拼一个 gate 出来，
//! 恰恰把这三点全省掉了。
//!
//! 要钉住的判定（docs/24 §5.1 / docs/14 §8.1）：
//!
//! 1. **闸门早于副作用**：未安装 / 未装载 / 未声明 / 入参畸形 / 路径越界，一律在
//!    插件被调用之前就回错——用例里插件每收到一次 `plugin/command-invoke` 就追加
//!    一行标记，标记文件没长才算真的没执行；
//! 2. **写命令必须过审批**：不弹审批就落盘，等于 `risk=write` 形同虚设；
//! 3. **拒绝后无副作用，且审计留痕**：拒绝要有 `3001`、要落 `decision=reject`；
//!    批准要把 `risk` / `path` / `call_id` 一起记下来，否则事后查不出「谁动了哪个文件」；
//! 4. **「总是允许」按命令粒度记住**：批准一次就记住的是这条命令，不是整个插件。

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Core, TempDataDir};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 命令替身插件：每收到一次 `plugin/command-invoke` 就往标记文件追加一行。
///
/// 三条命令各司其职：`peek_cmd` 只读、`poke_cmd` 写盘且带 `pathField`、
/// `big_cmd` 吐 70 KB（用来验 64 KiB 截断）。
const PLUGIN_SCRIPT: &str = r#"
const fs = require('node:fs')
const path = require('node:path')
const readline = require('node:readline')

const marker = path.join(__dirname, 'commands.log')

const TOOLS = [
  { name: 'poke_tool', description: '写盘工具（给 fs.write 一个正当用途）', risk: 'write',
    parameters: { type: 'object', properties: {} } }
]

const COMMANDS = [
  { name: 'peek_cmd', title: '看一眼', risk: 'read',
    args: { type: 'object', properties: { q: { type: 'string' } },
            required: ['q'], additionalProperties: false } },
  { name: 'poke_cmd', title: '动一下', risk: 'write', pathField: 'out',
    args: { type: 'object', properties: { out: { type: 'string' } },
            required: ['out'], additionalProperties: false } },
  { name: 'big_cmd', title: '吐一大段', risk: 'read',
    args: { type: 'object', properties: {} } }
]

function send(frame) {
  process.stdout.write(JSON.stringify(frame) + '\n')
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
rl.on('line', (line) => {
  if (!line.trim()) return
  const frame = JSON.parse(line)
  const params = frame.params || {}

  if (frame.method === 'plugin/handshake' || frame.method === 'plugin/initialize') {
    send({ jsonrpc: '2.0', id: frame.id, result: {
      pluginApi: { major: 1, minor: 0 },
      name: '命令演示插件',
      version: '0.1.0',
      tools: TOOLS,
      commands: COMMANDS,
      subscriptions: []
    } })
    return
  }

  if (frame.method === 'plugin/shutdown') {
    send({ jsonrpc: '2.0', id: frame.id, result: {} })
    setTimeout(() => process.exit(0), 10)
    return
  }

  if (frame.method === 'plugin/command-invoke') {
    fs.appendFileSync(marker, JSON.stringify({ command: params.command, args: params.args || {} }) + '\n')
    if (params.command === 'big_cmd') {
      send({ jsonrpc: '2.0', id: frame.id, result: 'x'.repeat(70000) })
      return
    }
    send({ jsonrpc: '2.0', id: frame.id, result: 'cmd ' + params.command + ' ok' })
    return
  }

  // 心跳等宿主内部方法：回个空结果即可（命令网关不依赖它们）。
  send({ jsonrpc: '2.0', id: frame.id, result: {} })
})
"#;

/// 一份声明了三条命令的清单。
///
/// `capabilities: ["fs.write"]` 是 `poke_cmd`（`risk=write`）的前提：清单不自洽
/// 会在安装期就 6107，测不到命令网关。
fn manifest(id: &str) -> Value {
    json!({
        "id": id,
        "name": "命令演示插件",
        "version": "0.1.0",
        "description": "plugin.command 端到端测试用插件",
        "pluginApi": "1.0",
        "main": "index.cjs",
        "capabilities": ["fs.write"],
        "tools": [
            {
                "name": "poke_tool",
                "description": "写盘工具（给 fs.write 一个正当用途）",
                "risk": "write",
                "parameters": { "type": "object", "properties": {} }
            }
        ],
        "commands": [
            {
                "name": "peek_cmd",
                "title": "看一眼",
                "description": "只读命令，不该弹审批",
                "risk": "read",
                "args": {
                    "type": "object",
                    "properties": { "q": { "type": "string" } },
                    "required": ["q"],
                    "additionalProperties": false
                }
            },
            {
                "name": "poke_cmd",
                "title": "动一下",
                "description": "写盘命令，执行前必须弹审批",
                "risk": "write",
                "pathField": "out",
                "args": {
                    "type": "object",
                    "properties": { "out": { "type": "string" } },
                    "required": ["out"],
                    "additionalProperties": false
                }
            },
            {
                "name": "big_cmd",
                "title": "吐一大段",
                "description": "回 70 KB，验截断",
                "risk": "read",
                "args": { "type": "object", "properties": {} }
            }
        ],
        "subscriptions": []
    })
}

/// 造一个插件包目录，返回包根。
fn write_package(root: &Path, id: &str) -> PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).expect("建包目录");
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest(id)).expect("序列化清单"),
    )
    .expect("写清单");
    std::fs::write(dir.join("index.cjs"), PLUGIN_SCRIPT).expect("写入口");
    dir
}

fn node_ready() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// 一次装好 + 授权 + 装载的现场。
///
/// 审批链路要求「有活动工作区」：`PluginWriteApprover` 没有 root 就直接判拒，
/// `pathField` 的路径闸门也要拿工作区当基准，所以工作区必须先开。
struct Fixture {
    core: Core,
    /// 数据目录：插件安装目录在它下面的 `plugins/<id>`。
    ///
    /// 声明在 `core` 之后，是为了先杀进程再删目录（字段按声明顺序 drop）。
    data: TempDataDir,
    ws: TempDataDir,
    packages: TempDataDir,
    plugin_id: String,
}

impl Fixture {
    /// `grant` 为 false 时停在「装好待授权」，用于验证 6102 / 装载期拦截。
    fn boot(tag: &str, grant: bool) -> Self {
        let data = TempDataDir::new(&format!("cmd-{tag}"));
        let ws = TempDataDir::new(&format!("cmd-ws-{tag}"));
        let packages = TempDataDir::new(&format!("cmd-pkg-{tag}"));
        let mut core = Core::spawn(&data.root);
        core.ok(
            "workspace.open",
            json!({ "path": ws.root.to_string_lossy(), "watch": false }),
        );
        let package = write_package(&packages.root, "cmd-demo");
        core.ok(
            "plugin.install",
            json!({ "path": package.to_string_lossy() }),
        );
        if grant {
            core.ok(
                "plugin.grant",
                json!({ "plugin_id": "cmd-demo", "capabilities": ["fs.write"] }),
            );
            core.ok("plugin.load", json!({ "plugin_id": "cmd-demo" }));
        }
        Self {
            core,
            data,
            ws,
            packages,
            plugin_id: "cmd-demo".to_string(),
        }
    }

    /// 插件被真正执行过的证据：**安装目录**里的标记文件。
    ///
    /// 读数据目录下的 `plugins/<id>` 而不是源包：插件进程的 `__dirname` 指向那里。
    fn marker(&self) -> PathBuf {
        self.data
            .root
            .join("plugins")
            .join(&self.plugin_id)
            .join("commands.log")
    }

    fn marker_lines(&self) -> Vec<String> {
        match std::fs::read_to_string(self.marker()) {
            Ok(text) => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 发一条命令，期望报错，返回协议错误码。
    fn command_error(&mut self, command: &str, args: Value) -> i64 {
        self.core.error_code(
            "plugin.command",
            json!({ "plugin_id": self.plugin_id, "command": command, "args": args }),
        )
    }

    /// 发一条会弹审批的命令，等审批事件 → 按 `decision` 回执 → 等最终响应。
    fn command_with_approval(&mut self, command: &str, args: Value, decision: &str) -> Value {
        let id = self.core.call_async(
            "plugin.command",
            json!({ "plugin_id": self.plugin_id, "command": command, "args": args }),
        );
        let event = self.core.wait_notification("agent.approval_required");
        let params = &event["params"];
        let run_id = params["run_id"].as_str().expect("审批事件带 run_id");
        let approval_id = params["approval_id"].as_str().expect("审批事件带 approval_id");
        self.core.ok(
            "agent.decide",
            json!({ "run_id": run_id, "approval_id": approval_id, "decision": decision }),
        );
        self.core.await_response(&id)
    }

    /// 拉某个 kind 的审计事件。
    fn audit(&mut self, kind: &str) -> Vec<Value> {
        self.core.ok("audit.list", json!({ "kind": kind }))["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 闸门 ①–④：畸形参数、未安装、未装载、未声明——全在碰插件之前回错。
#[test]
fn gates_reject_before_the_plugin_is_ever_called() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let mut fx = Fixture::boot("gates", false);

    // ① 空命令名 / 空插件 id：-32602，且不碰库。
    assert_eq!(
        fx.command_error("   ", json!({})),
        -32602,
        "空命令名应是 -32602"
    );
    assert_eq!(
        fx.core.error_code(
            "plugin.command",
            json!({ "plugin_id": "  ", "command": "peek_cmd" })
        ),
        -32602,
        "空插件 id 应是 -32602"
    );
    // args 必须是对象：数组会让插件那边炸出更难懂的错误。
    assert_eq!(
        fx.core.error_code(
            "plugin.command",
            json!({ "plugin_id": "cmd-demo", "command": "peek_cmd", "args": ["q"] })
        ),
        -32602,
        "非对象 args 应是 -32602"
    );

    // ② 装好但没装载：6102（命令要打到进程上）。
    assert_eq!(
        fx.command_error("peek_cmd", json!({ "q": "hi" })),
        6102,
        "未装载应是 6102"
    );

    // 装好待授权时 load 会被拦（缺能力），这本身也是 6102 的一种来源。
    let blocked = fx
        .core
        .call("plugin.load", json!({ "plugin_id": "cmd-demo" }));
    assert!(
        blocked.get("error").is_some(),
        "capabilities 申请了 fs.write 却未授权，装载应被拦下：{blocked}"
    );

    // ③ 授权 + 装载之后再问「不存在的命令」：6106（归属）。
    fx.core.ok(
        "plugin.grant",
        json!({ "plugin_id": "cmd-demo", "capabilities": ["fs.write"] }),
    );
    fx.core.ok("plugin.load", json!({ "plugin_id": "cmd-demo" }));
    assert_eq!(
        fx.command_error("nope_cmd", json!({})),
        6106,
        "未声明的命令应是 6106"
    );

    // 未安装的插件 id：6101。
    assert_eq!(
        fx.core.error_code(
            "plugin.command",
            json!({ "plugin_id": "ghost", "command": "peek_cmd" })
        ),
        6101,
        "未安装应是 6101"
    );

    assert!(
        fx.marker_lines().is_empty(),
        "闸门全部在插件之前拦下，标记文件不该被创建：{:?}",
        fx.marker_lines()
    );
}

/// 只读命令免问；写了会按 `allow_once` 落一次，且标记文件真的长了一行。
#[test]
fn read_command_runs_quietly_and_write_command_needs_a_yes() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let mut fx = Fixture::boot("approve", true);

    // 只读：直接拿结果，中途不该出现审批事件。
    let quiet = fx.core.ok(
        "plugin.command",
        json!({ "plugin_id": "cmd-demo", "command": "peek_cmd", "args": { "q": "hi" } }),
    );
    assert_eq!(quiet["content"], json!("cmd peek_cmd ok"), "回包应透出插件正文");
    assert_eq!(quiet["truncated"], json!(false));
    assert!(
        fx.core.events_named("agent.approval_required").is_empty(),
        "只读命令不该弹审批"
    );

    // 写入：必须先问，批准后才转发。
    let response = fx.command_with_approval("poke_cmd", json!({ "out": "out.txt" }), "allow_once");
    let result = response.get("result").expect("批准后应有结果");
    assert_eq!(result["content"], json!("cmd poke_cmd ok"));
    assert_eq!(result["command"], json!("poke_cmd"));

    let lines = fx.marker_lines();
    assert_eq!(lines.len(), 2, "只读一次 + 写入一次：{lines:?}");
    assert!(
        lines[1].contains("poke_cmd") && lines[1].contains("out.txt"),
        "插件收到的写入命令参数应原样透传：{}",
        lines[1]
    );

    // 审计：批准一次要留下 risk / path / decision，否则事后查不出动了哪个文件。
    // 用户当场批准记 `approve`（docs/14 §8.3）：`allow` 留给「命中免问规则、这次
    // 压根没问人」的情形，两者混在一起就分不清这条命令到底有没有经过用户。
    let events = fx.audit("plugin.command");
    let approved = events
        .iter()
        .find(|event| event["detail"]["decision"] == json!("approve"))
        .unwrap_or_else(|| panic!("缺少 approve 审计：{events:?}"));
    assert_eq!(approved["detail"]["risk"], json!("write"));
    assert_eq!(approved["detail"]["target"], json!("poke_cmd"));
    let path = approved["detail"]["path"]
        .as_str()
        .expect("写命令审计要带 path");
    assert!(
        path.ends_with("out.txt"),
        "审计里的 path 应是解析后的绝对路径：{path}"
    );
    assert!(
        path.starts_with(&*fx.ws.root.to_string_lossy()),
        "解析结果必须落在工作区内：{path}"
    );
}

/// 拒绝：回 3001、标记文件不动、审计留 `reject`。
#[test]
fn denied_write_command_leaves_no_trace_in_the_plugin() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let mut fx = Fixture::boot("deny", true);

    let response = fx.command_with_approval("poke_cmd", json!({ "out": "nope.txt" }), "deny");
    let error = response.get("error").expect("拒绝后应回错");
    assert_eq!(error["code"], json!(3001), "拒绝应是 3001：{response}");

    assert!(
        fx.marker_lines().is_empty(),
        "拒绝后插件一次都不该被调用：{:?}",
        fx.marker_lines()
    );

    let events = fx.audit("plugin.command");
    let rejected = events
        .iter()
        .find(|event| event["detail"]["decision"] == json!("reject"))
        .unwrap_or_else(|| panic!("缺少 reject 审计：{events:?}"));
    assert_eq!(rejected["detail"]["risk"], json!("write"));
}

/// 入参 schema 与路径闸门都排在审批**之前**：不该为一次畸形请求打扰用户。
#[test]
fn args_and_path_gates_run_before_the_approval_prompt() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let mut fx = Fixture::boot("gates2", true);

    // 缺必填字段：-32602。
    assert_eq!(
        fx.command_error("poke_cmd", json!({})),
        -32602,
        "缺 required 应是 -32602"
    );
    // 多给字段：additionalProperties: false → -32602。
    assert_eq!(
        fx.command_error("poke_cmd", json!({ "out": "a.txt", "extra": 1 })),
        -32602,
        "未声明字段应是 -32602"
    );
    // 路径越界：1002，且同样不弹审批。
    assert_eq!(
        fx.command_error("poke_cmd", json!({ "out": "../escape.txt" })),
        1002,
        "越界路径应是 1002"
    );

    assert!(
        fx.core.events_named("agent.approval_required").is_empty(),
        "畸形请求不该弹审批框"
    );
    assert!(fx.marker_lines().is_empty(), "上述请求一条都不该转发给插件");
}

/// 「总是允许」是按**命令**记的：批一次 `poke_cmd`，第二次不再问，
/// 但换成别的写命令仍然要问（这里用同一条命令 + 不同路径验证粒度含路径）。
#[test]
fn allow_always_is_remembered_per_command() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let mut fx = Fixture::boot("always", true);

    let first = fx.command_with_approval("poke_cmd", json!({ "out": "a.txt" }), "allow_always");
    assert!(
        first.get("result").is_some(),
        "首次批准应成功：{first}"
    );

    // 同一条命令 + 同一个路径：不该再弹审批。
    let again = fx.core.call(
        "plugin.command",
        json!({ "plugin_id": "cmd-demo", "command": "poke_cmd", "args": { "out": "a.txt" } }),
    );
    assert!(
        again.get("result").is_some(),
        "记住之后应直接执行：{again}"
    );
    assert!(
        fx.core.events_named("agent.approval_required").len() <= 1,
        "「总是允许」之后不该再产生审批事件"
    );

    // 换一个路径：粒度是 `plugin.command:<id>:<cmd>:<path>`，仍要问。
    let id = fx.core.call_async(
        "plugin.command",
        json!({ "plugin_id": "cmd-demo", "command": "poke_cmd", "args": { "out": "b.txt" } }),
    );
    let event = fx.core.wait_notification("agent.approval_required");
    let params = &event["params"];
    fx.core.ok(
        "agent.decide",
        json!({
            "run_id": params["run_id"],
            "approval_id": params["approval_id"],
            "decision": "deny"
        }),
    );
    let denied = fx.core.await_response(&id);
    assert_eq!(
        denied["error"]["code"],
        json!(3001),
        "另一个路径仍需审批，拒后应 3001：{denied}"
    );

    assert_eq!(
        fx.marker_lines().len(),
        2,
        "只有两条被批准的命令真的执行了"
    );
}

/// 结果超 64 KiB：截断并打标，调用本身仍算成功。
#[test]
fn oversized_result_is_truncated_not_failed() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let mut fx = Fixture::boot("truncate", true);

    let result = fx.core.ok(
        "plugin.command",
        json!({ "plugin_id": "cmd-demo", "command": "big_cmd", "args": {} }),
    );
    assert_eq!(result["truncated"], json!(true), "70 KB 应被判截断");
    let content = result["content"].as_str().expect("content 是字符串");
    assert!(
        content.len() <= 64 * 1024,
        "截断后不该超过 64 KiB，实际 {}",
        content.len()
    );
    assert!(!content.is_empty(), "截断不等于清空");

    let events = fx.audit("plugin.command");
    assert!(
        events
            .iter()
            .any(|event| event["detail"]["truncated"] == json!(true)),
        "截断要在审计里留痕：{events:?}"
    );
}

/// 命令目录只收**跑着的**插件：卸载后同一命令立刻回到 6101。
#[test]
fn unloading_takes_the_command_away_immediately() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let mut fx = Fixture::boot("unload", true);

    // 跑着的时候是可调的。
    let ok = fx.core.ok(
        "plugin.command",
        json!({ "plugin_id": "cmd-demo", "command": "peek_cmd", "args": { "q": "hi" } }),
    );
    assert_eq!(ok["content"], json!("cmd peek_cmd ok"));

    fx.core.ok("plugin.unload", json!({ "plugin_id": "cmd-demo" }));
    assert_eq!(
        fx.command_error("peek_cmd", json!({ "q": "hi" })),
        6102,
        "卸载后命令立即不可路由"
    );

    fx.core.ok("plugin.uninstall", json!({ "plugin_id": "cmd-demo" }));
    assert_eq!(
        fx.command_error("peek_cmd", json!({ "q": "hi" })),
        6101,
        "卸载后应回到 6101"
    );

    // 提示：卸载会删安装目录，标记文件随之消失；这里只断言「不再有新执行」。
    let _ = Duration::from_secs(0);
}
