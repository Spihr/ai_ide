//! 宿主装载/握手的端到端行为（真起 node 子进程，不 mock 协议）。
//!
//! 这里只验插件 crate 自己的承诺：授权不齐不起进程、握手不合规不留半装载状态、
//! 崩溃即下架、卸载即关停、重复 load 幂等。
//! 内核账本与 61xx 映射由 `crates/core/tests/plugin_flow.rs` 负责。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use ai_ide_plugin::{
    host::{Handshake, Host, LoadError, LoadSpec, Restart, MAX_AUTO_RESTARTS},
    manifest::validate,
    ValidatedManifest,
};
use serde_json::{json, Value};

fn node_ready() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|gap| gap.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "aiide-host-{tag}-{}-{unique}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 造一个插件包目录并校验出清单。
fn package(dir: &Path, tools: &[&str], entry_source: &str) -> ValidatedManifest {
    let manifest = json!({
        "id": "demo-node",
        "name": "Demo Node",
        "version": "0.1.0",
        "pluginApi": "1.0",
        "main": "index.cjs",
        "capabilities": ["fs.read"],
        "tools": tools.iter().map(|name| json!({
            "name": name,
            "description": format!("{name} 的说明"),
            "risk": "read",
            "parameters": { "type": "object" },
        })).collect::<Vec<_>>(),
        "commands": [],
        "subscriptions": [],
    });
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest).expect("序列化清单"),
    )
    .expect("写清单");
    std::fs::write(dir.join("index.cjs"), entry_source).expect("写入口");
    validate(dir, &manifest).expect("清单应合法")
}

/// 一个会握手、并可选「过一会儿自己死」的插件脚本。
fn script(tools: &[&str], api_major: u32, die_after_ms: Option<u64>) -> String {
    let die = die_after_ms
        .map(|ms| format!("setTimeout(() => process.exit(7), {ms});"))
        .unwrap_or_default();
    script_with(tools, api_major, &die)
}

/// 带「失败模式」的插件脚本：用入口目录里的 `.starts` 文件数每次启动次数，
/// 好让测试脚本化地表达「只崩第一次」「每次起都崩」「第一次崩、之后握手漂移」。
///
/// - `recover`：第一次握手成功后自杀，之后正常常驻（用来验自动恢复）；
/// - `dead`：除第一次外每次启动立刻退出（用来验重启到顶）；
/// - `drift`：第一次握手成功后自杀，之后握手多报一个清单里没有的工具
///   （确定性失败，用来验不重试）。
fn stateful_script(tools: &[&str], mode: &str) -> String {
    let head = format!(
        r#"const fs = require('node:fs')
const path = require('node:path')
const MODE = {mode:?}
const counterFile = path.join(__dirname, '.starts')
let seen = 0
try {{ seen = parseInt(fs.readFileSync(counterFile, 'utf8'), 10) || 0 }} catch (e) {{}}
seen += 1
fs.writeFileSync(counterFile, String(seen))
if (MODE === 'dead' && seen > 1) {{ process.exit(7) }}
"#
    );
    // 每次只崩第一次：握手落定后再退，崩溃检测才有机会看到「装着装着就没了」。
    let die = "if (seen === 1) { setTimeout(() => process.exit(7), 120); }";
    let base = script_with(tools, 1, die);
    if mode == "drift" {
        // 在握手回包的工具列表后面塞一个清单里没有的工具（靠追加到数组实现）。
        let drift = base.replace(
            "tools: TOOLS,",
            "tools: seen > 1 ? TOOLS.concat([{ name: 'ghost', description: '清单里没有的工具', risk: 'read', parameters: { type: 'object' } }]) : TOOLS,",
        );
        format!("{head}{drift}")
    } else {
        format!("{head}{base}")
    }
}

fn script_with(tools: &[&str], api_major: u32, die: &str) -> String {
    let tool_list = tools
        .iter()
        .map(|name| {
            format!(
                "{{ name: {name:?}, description: \"{name} 回显\", risk: 'read', parameters: {{ type: 'object' }} }}"
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
const readline = require('node:readline')
const TOOLS = [{tool_list}]
const rl = readline.createInterface({{ input: process.stdin, crlfDelay: Infinity }})
function send(frame) {{ process.stdout.write(JSON.stringify(frame) + '\n') }}
rl.on('line', (line) => {{
  if (!line.trim()) return
  const frame = JSON.parse(line)
  if (frame.method === 'plugin/handshake' || frame.method === 'plugin/initialize') {{
    send({{ jsonrpc: '2.0', id: frame.id, result: {{
      pluginApi: {{ major: {api_major}, minor: 0 }},
      name: 'Demo Node',
      version: '0.1.0',
      tools: TOOLS,
      commands: [],
      subscriptions: []
    }} }})
    {die}
  }} else if (frame.method === 'plugin/shutdown') {{
    send({{ jsonrpc: '2.0', id: frame.id, result: {{}} }})
    setTimeout(() => process.exit(0), 10)
  }} else {{
    send({{ jsonrpc: '2.0', id: frame.id, error: {{ code: -32601, message: '未实现：' + frame.method }} }})
  }}
}})
"#
    )
}

fn spec<'a>(dir: &'a Path, validated: &'a ValidatedManifest, granted: &[&str]) -> LoadSpec<'a> {
    LoadSpec {
        plugin_id: "demo-node",
        install_dir: dir,
        validated,
        requested: validated.manifest.capabilities.clone(),
        granted: granted.iter().map(|value| value.to_string()).collect(),
        workspace: Value::Null,
    }
}

#[tokio::test]
async fn load_registers_tools_and_is_idempotent() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("load");
    let validated = package(
        &dir,
        &["echo", "lookup"],
        &script(&["echo", "lookup"], 1, None),
    );
    let host = Host::new();

    let first = host
        .load(spec(&dir, &validated, &["fs.read"]))
        .await
        .expect("应装载成功");
    assert!(!first.reused);
    assert_eq!(
        first.namespaced_tools,
        vec![
            "plugin__demo-node__echo".to_string(),
            "plugin__demo-node__lookup".to_string()
        ]
    );
    assert!(first.pid.is_some(), "应拿到子进程 pid");

    let again = host
        .load(spec(&dir, &validated, &["fs.read"]))
        .await
        .expect("重复装载应复用");
    assert!(again.reused, "第二次 load 应复用已在跑的进程");
    assert_eq!(again.pid, first.pid, "复用不能换进程");

    let removed = host.unload("demo-node").await.expect("应卸载成功");
    assert_eq!(removed.len(), 2);
    assert!(!host.is_running("demo-node"), "卸载后不应再算在跑");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn missing_capability_blocks_before_any_process_exists() {
    let dir = scratch("grant");
    // 入口故意不存在：只要它被碰过，报错就会变成 NotInstalled。
    let validated = package(&dir, &["echo"], &script(&["echo"], 1, None));
    std::fs::remove_file(dir.join("index.cjs")).expect("删入口");
    let host = Host::new();

    let mut load_spec = spec(&dir, &validated, &["fs.read"]);
    load_spec.requested = vec!["fs.read".to_string(), "fs.write".to_string()];
    match host.load(load_spec).await {
        Err(LoadError::MissingCapabilities { missing }) => {
            assert_eq!(missing, vec!["fs.write".to_string()]);
        }
        other => panic!("缺授权应在起进程前拦住，实际 {other:?}"),
    }
    assert!(!host.is_running("demo-node"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn tool_set_mismatch_leaves_nothing_registered() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("mismatch");
    // 清单声明 echo + lookup，握手只报 echo + ghost。
    let validated = package(
        &dir,
        &["echo", "lookup"],
        &script(&["echo", "ghost"], 1, None),
    );
    let host = Host::new();

    match host.load(spec(&dir, &validated, &["fs.read"])).await {
        Err(LoadError::Protocol { message }) => {
            assert!(
                message.contains("lookup"),
                "应点出清单有而未声明：{message}"
            );
            assert!(message.contains("ghost"), "应点出多报的工具：{message}");
        }
        other => panic!("工具集合不一致必须拒绝装载，实际 {other:?}"),
    }
    assert!(!host.is_running("demo-node"), "失败不能留下半装载状态");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn api_major_mismatch_is_reported() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("api");
    let validated = package(&dir, &["echo"], &script(&["echo"], 2, None));
    let host = Host::new();

    match host.load(spec(&dir, &validated, &["fs.read"])).await {
        Err(LoadError::ApiMismatch {
            plugin,
            host: host_api,
        }) => {
            assert_eq!(plugin, "2.0");
            assert_eq!(host_api, "1.0");
        }
        other => panic!("API major 不等必须拒绝，实际 {other:?}"),
    }
    assert!(!host.is_running("demo-node"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn crashed_plugin_is_reaped_and_reported() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("crash");
    let validated = package(&dir, &["echo"], &script(&["echo"], 1, Some(150)));
    let host = Host::new();

    let loaded = host
        .load(spec(&dir, &validated, &["fs.read"]))
        .await
        .expect("握手应先成功");
    assert!(loaded.pid.is_some());

    // 等插件自己退出：崩溃检测靠 stdout EOF，不是靠轮询 pid。
    let mut reaped = Vec::new();
    for _ in 0..100 {
        reaped = host.take_crashed();
        if !reaped.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(reaped.len(), 1, "崩掉的插件应被收走");
    assert_eq!(reaped[0].plugin_id, "demo-node");
    assert_eq!(
        reaped[0].namespaced_tools,
        vec!["plugin__demo-node__echo".to_string()],
        "下架清单要能告诉上层摘掉哪些工具"
    );
    assert!(!host.is_running("demo-node"), "崩溃后不应再算在跑");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 等宿主看到「进程已退出」，最多 ~3s；拿不到就返回空。
async fn wait_crashed(host: &Host) -> Vec<String> {
    for _ in 0..100 {
        let ids = host.crashed_ids();
        if !ids.is_empty() {
            return ids;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    Vec::new()
}

#[tokio::test]
async fn crash_then_auto_restart_brings_tools_back() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("restart-ok");
    let validated = package(&dir, &["echo"], &stateful_script(&["echo"], "recover"));
    let host = Host::new();

    let first = host
        .load(spec(&dir, &validated, &["fs.read"]))
        .await
        .expect("首装应握手成功");
    assert!(first.pid.is_some());

    let crashed = wait_crashed(&host).await;
    assert_eq!(crashed, vec!["demo-node".to_string()], "进程应被判死");
    // 崩溃的那一刻工具就不再可路由：这是「崩溃 → 工具立即下架」。
    assert!(
        host.running_tools("demo-node").is_empty(),
        "崩溃后不应还有工具在架上"
    );

    match host.auto_restart("demo-node").await {
        Restart::Recovered { loaded, attempts } => {
            assert_eq!(attempts, 1, "第二次启动就成功了，只该用掉 1 次预算");
            assert!(!loaded.reused, "自动重启是新起的进程，不是复用");
            assert_ne!(loaded.pid, first.pid, "重启必须换一个新进程");
            assert_eq!(
                loaded.namespaced_tools,
                vec!["plugin__demo-node__echo".to_string()],
                "重启后工具要重新上架"
            );
        }
        other => panic!("期望自动重启成功，实际 {other:?}"),
    }
    assert!(host.is_running("demo-node"), "重启成功后应算在跑");
    assert_eq!(
        host.running_tools("demo-node"),
        vec!["plugin__demo-node__echo".to_string()],
        "重启后工具应可路由"
    );
    assert!(host.crashed_ids().is_empty(), "恢复后不该再被判死");

    host.shutdown_all().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn auto_restart_stops_at_limit_and_says_so() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("restart-limit");
    let validated = package(&dir, &["echo"], &stateful_script(&["echo"], "dead"));
    let host = Host::new();

    host.load(spec(&dir, &validated, &["fs.read"]))
        .await
        .expect("首装应握手成功");
    assert_eq!(
        wait_crashed(&host).await,
        vec!["demo-node".to_string()],
        "进程应被判死"
    );

    match host.auto_restart("demo-node").await {
        Restart::Exhausted {
            attempts, message, ..
        } => {
            assert_eq!(attempts, MAX_AUTO_RESTARTS, "应把 3 次预算用满");
            assert!(
                message.contains("已达自动重启上限"),
                "到顶原因要能直接写进 last_error，实际：{message}"
            );
        }
        other => panic!("期望重试用尽，实际 {other:?}"),
    }
    assert!(!host.is_running("demo-node"), "到顶后应停在已崩溃");
    assert!(
        host.running_tools("demo-node").is_empty(),
        "到顶后工具不能留在架上"
    );
    assert!(
        host.crashed_ids().is_empty(),
        "到顶后条目已从装载表摘除，不该再被巡检到"
    );

    // 首装 1 次 + 重试 3 次 = 4 次启动；一次都不许多。
    let started: u32 = std::fs::read_to_string(dir.join(".starts"))
        .expect("读启动计数")
        .trim()
        .parse()
        .expect("计数应是整数");
    assert_eq!(started, 1 + MAX_AUTO_RESTARTS, "重启次数不得越过上限");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn deterministic_handshake_failure_is_not_retried() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("restart-deterministic");
    let validated = package(&dir, &["echo"], &stateful_script(&["echo"], "drift"));
    let host = Host::new();

    host.load(spec(&dir, &validated, &["fs.read"]))
        .await
        .expect("首装应握手成功");
    assert_eq!(
        wait_crashed(&host).await,
        vec!["demo-node".to_string()],
        "进程应被判死"
    );

    match host.auto_restart("demo-node").await {
        Restart::Deterministic { error, attempts } => {
            assert_eq!(attempts, 1, "确定性失败只试第一次，不该继续重试");
            assert!(
                matches!(error, LoadError::Protocol { .. }),
                "工具集合与清单不一致应判为协议错误，实际 {error:?}"
            );
        }
        other => panic!("期望判为确定性失败，实际 {other:?}"),
    }
    assert!(!host.is_running("demo-node"), "不重试就该停在已崩溃");

    // 只启动过 2 次（首装 + 唯一一次重启尝试）：重启循环必须立刻刹车。
    let started: u32 = std::fs::read_to_string(dir.join(".starts"))
        .expect("读启动计数")
        .trim()
        .parse()
        .expect("计数应是整数");
    assert_eq!(started, 2, "确定性失败后不得再起进程");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn handshake_falls_back_to_legacy_initialize() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("legacy");
    // 只实现老方法名 plugin/initialize 的插件。
    let body = r#"
const readline = require('node:readline')
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
function send(frame) { process.stdout.write(JSON.stringify(frame) + '\n') }
rl.on('line', (line) => {
  if (!line.trim()) return
  const frame = JSON.parse(line)
  if (frame.method === 'plugin/initialize') {
    send({ jsonrpc: '2.0', id: frame.id, result: {
      pluginApi: '1.0', name: 'Legacy', version: '0.1.0',
      tools: [{ name: 'echo', description: '回显', risk: 'read', parameters: { type: 'object' } }],
      commands: [], subscriptions: []
    } })
  } else if (frame.method === 'plugin/handshake') {
    send({ jsonrpc: '2.0', id: frame.id, error: { code: -32601, message: 'no such method' } })
  } else if (frame.method === 'plugin/shutdown') {
    send({ jsonrpc: '2.0', id: frame.id, result: {} })
    setTimeout(() => process.exit(0), 10)
  }
})
"#;
    let validated = package(&dir, &["echo"], body);
    let host = Host::new();

    let loaded = host
        .load(spec(&dir, &validated, &["fs.read"]))
        .await
        .expect("应退回 plugin/initialize 并握手成功");
    let handshake: &Handshake = &loaded.handshake;
    assert_eq!(handshake.name.as_deref(), Some("Legacy"));
    assert_eq!(handshake.tool_names(), vec!["echo".to_string()]);
    host.shutdown_all().await;
    let _ = std::fs::remove_dir_all(&dir);
}
