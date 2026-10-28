//! 宿主装载/握手的端到端行为（真起 node 子进程，不 mock 协议）。
//!
//! 这里只验插件 crate 自己的承诺：授权不齐不起进程、握手不合规不留半装载状态、
//! 崩溃即下架、卸载即关停、重复 load 幂等。
//! 内核账本与 61xx 映射由 `crates/core/tests/plugin_flow.rs` 负责。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
            // 宿主能力集写死在这里是有意的：minor 变了就说明 `plugin.status` 的
            // 字段集变了（1.1 起多下发 `views`），这条断言逼着人回来确认一次。
            assert_eq!(host_api, "1.1");
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

// ---------------------------------------------------------------------------
// P5 切片八：多插件并发装载（互斥 / 隔离 / 广播独立）
// ---------------------------------------------------------------------------

/// 可编排的探针插件：三个可观测点，全部落在插件包里，测试直接读文件断言。
///
/// 1. `.starts`：本进程被启动了几次（每次启动 +1）——用来证明「同一 id 并发
///    load 只起一个进程」，比看 `pid` 更硬：pid 复用/覆盖时 pid 也可能相等；
/// 2. `.timeline`：`start <ms>` / `done <ms>` 两条记录——用来证明多个插件是
///    **真的同时在等握手**，而不是排着队一个个来（时间窗重叠）；
/// 3. `events.log`：收到的 `plugin/event` 通知逐条追加——用来证明事件广播只到
///    订阅了该事件的插件，不串台。
fn probe_script(tools: &[&str], delay_ms: u64, subscriptions: &[&str]) -> String {
    let tool_list = tools
        .iter()
        .map(|name| {
            format!(
                "{{ name: {name:?}, description: {name:?}, risk: 'read', parameters: {{ type: 'object' }} }}"
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sub_list = subscriptions
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
const fs = require('node:fs')
const path = require('node:path')
const readline = require('node:readline')
const TOOLS = [{tool_list}]
const SUBS = [{sub_list}]
const startsFile = path.join(__dirname, '.starts')
const timelineFile = path.join(__dirname, '.timeline')
const eventsFile = path.join(__dirname, 'events.log')

let seen = 0
try {{ seen = parseInt(fs.readFileSync(startsFile, 'utf8'), 10) || 0 }} catch (e) {{}}
seen += 1
fs.writeFileSync(startsFile, String(seen))
fs.appendFileSync(timelineFile, 'start ' + Date.now() + '\n')

const rl = readline.createInterface({{ input: process.stdin, crlfDelay: Infinity }})
function send(frame) {{ process.stdout.write(JSON.stringify(frame) + '\n') }}
rl.on('line', (line) => {{
  if (!line.trim()) return
  const frame = JSON.parse(line)
  if (frame.method === 'plugin/handshake' || frame.method === 'plugin/initialize') {{
    // 故意慢一点回握手：并发装载的证据就是「多个插件的等待窗口重叠」。
    setTimeout(() => {{
      send({{ jsonrpc: '2.0', id: frame.id, result: {{
        pluginApi: {{ major: 1, minor: 0 }},
        name: 'Probe',
        version: '0.1.0',
        tools: TOOLS,
        commands: [],
        subscriptions: SUBS
      }} }})
      fs.appendFileSync(timelineFile, 'done ' + Date.now() + '\n')
    }}, {delay_ms})
  }} else if (frame.method === 'plugin/event') {{
    fs.appendFileSync(eventsFile, JSON.stringify(frame.params) + '\n')
  }} else if (frame.method === 'plugin/shutdown') {{
    send({{ jsonrpc: '2.0', id: frame.id, result: {{}} }})
    setTimeout(() => process.exit(0), 10)
  }} else {{
    send({{ jsonrpc: '2.0', id: frame.id, error: {{ code: -32601, message: 'unimplemented: ' + frame.method }} }})
  }}
}})
"#
    )
}

/// 起一个立刻崩掉的入口：进程起来就退，装载必失败，且不是「清单不合法」那种
/// 前置失败——用它验证「失败发生在装载过程中，其他插件照样能装完」。
fn crashing_script() -> String {
    "process.exit(3)\n".to_string()
}

/// 按指定 id / 订阅清单造插件包（`package()` 把 id 写死成 demo-node，这里需要多
/// 个不同 id 同场）。
fn package_for(
    dir: &Path,
    id: &str,
    tools: &[&str],
    subscriptions: &[&str],
    entry_source: &str,
) -> ValidatedManifest {
    let manifest = json!({
        "id": id,
        "name": format!("Probe {id}"),
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
        "subscriptions": subscriptions,
    });
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest).expect("序列化清单"),
    )
    .expect("写清单");
    std::fs::write(dir.join("index.cjs"), entry_source).expect("写入口");
    validate(dir, &manifest).expect("清单应合法")
}

fn spec_for<'a>(
    dir: &'a Path,
    id: &'a str,
    validated: &'a ValidatedManifest,
    granted: &[&str],
) -> LoadSpec<'a> {
    LoadSpec {
        plugin_id: id,
        install_dir: dir,
        validated,
        requested: validated.manifest.capabilities.clone(),
        granted: granted.iter().map(|value| value.to_string()).collect(),
        workspace: Value::Null,
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn read_starts(dir: &Path) -> u32 {
    std::fs::read_to_string(dir.join(".starts"))
        .expect("读启动计数")
        .trim()
        .parse()
        .expect("计数应是整数")
}

/// `.timeline` 里的 `start` / `done` 时间戳（毫秒）。
fn read_window(dir: &Path) -> (u64, u64) {
    let mut start = None;
    let mut done = None;
    for line in read_lines(&dir.join(".timeline")) {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("start"), Some(value)) => start = value.parse::<u64>().ok(),
            (Some("done"), Some(value)) => done = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    (start.expect("应记录 start"), done.expect("应记录 done"))
}

#[tokio::test]
async fn same_id_concurrent_load_starts_exactly_one_process() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let dir = scratch("same-id");
    // 握手故意慢 400ms：没有 per-id 闸门时，第二个 load 会在这 400ms 里
    // 从 alive() 里看不到任何东西，于是也起一个进程。
    let validated = package_for(
        &dir,
        "demo-node",
        &["echo"],
        &[],
        &probe_script(&["echo"], 400, &[]),
    );
    let host = Host::new();

    let (first, second) = tokio::join!(
        host.load(spec_for(&dir, "demo-node", &validated, &["fs.read"])),
        host.load(spec_for(&dir, "demo-node", &validated, &["fs.read"])),
    );
    let first = first.expect("第一次装载应成功");
    let second = second.expect("并发第二次装载应复用而不是失败");

    assert_eq!(first.pid, second.pid, "同一 id 并发装载只能是同一个进程");
    assert_ne!(
        first.reused, second.reused,
        "两次都 reused 或都没 reused 说明互斥失效"
    );
    assert_eq!(
        read_starts(&dir),
        1,
        "插件进程只应被启动一次（每次启动都会把 .starts +1）"
    );
    assert_eq!(
        host.running_ids(),
        vec!["demo-node".to_string()],
        "装载表里只应有一个条目"
    );
    assert_eq!(
        host.running_tools("demo-node"),
        vec!["plugin__demo-node__echo".to_string()],
        "复用不能把工具表搞成重复项"
    );

    host.shutdown_all().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn different_plugins_load_concurrently_with_overlapping_handshakes() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let root = scratch("parallel");
    let ids = ["alpha", "beta", "gamma"];
    let tools: [&[&str]; 3] = [&["a_one", "a_two"], &["b_one"], &["c_one"]];

    let mut dirs = Vec::new();
    let mut validated = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).expect("建插件目录");
        validated.push(package_for(
            &dir,
            id,
            tools[index],
            &[],
            &probe_script(tools[index], 300, &[]),
        ));
        dirs.push(dir);
    }

    let host = Host::new();
    let (a, b, c) = tokio::join!(
        host.load(spec_for(&dirs[0], ids[0], &validated[0], &["fs.read"])),
        host.load(spec_for(&dirs[1], ids[1], &validated[1], &["fs.read"])),
        host.load(spec_for(&dirs[2], ids[2], &validated[2], &["fs.read"])),
    );
    for (id, loaded) in ids.iter().zip([a, b, c]) {
        let loaded = loaded.unwrap_or_else(|error| panic!("{id} 应装载成功，实际 {error:?}"));
        assert!(loaded.pid.is_some());
        assert!(!loaded.reused, "首次装载不该被当成复用");
    }

    // 隔离与冲突：三个插件同时在跑，工具名各带自己的前缀，谁也不占谁的位。
    let mut running = host.running_ids();
    running.sort();
    assert_eq!(running, vec!["alpha", "beta", "gamma"]);
    assert_eq!(
        host.running_tools("alpha"),
        vec![
            "plugin__alpha__a_one".to_string(),
            "plugin__alpha__a_two".to_string()
        ]
    );
    assert_eq!(
        host.running_tools("beta"),
        vec!["plugin__beta__b_one".to_string()]
    );
    assert_eq!(
        host.running_tools("gamma"),
        vec!["plugin__gamma__c_one".to_string()]
    );
    assert_eq!(host.tool_catalog().len(), 4, "工具总表应等于三个插件之和");

    // 并发证据：三个插件的握手等待窗口必须互相重叠。串行装载时，前一个的
    // `done` 一定早于后一个的 `start`，这里就必然失败。
    let windows: Vec<(u64, u64)> = dirs.iter().map(|dir| read_window(dir)).collect();
    let latest_start = windows
        .iter()
        .map(|(start, _)| *start)
        .max()
        .expect("有窗口");
    let earliest_done = windows.iter().map(|(_, done)| *done).min().expect("有窗口");
    assert!(
        latest_start < earliest_done,
        "三个握手窗口没有重叠（{windows:?}），说明装载是串行的而不是并发的"
    );
    for dir in &dirs {
        assert_eq!(read_starts(dir), 1, "每个插件只该启动一次");
    }

    host.shutdown_all().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn one_plugin_failure_does_not_disturb_the_others() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let root = scratch("isolate");
    let good_a = root.join("good-a");
    let broken = root.join("broken");
    let good_b = root.join("good-b");
    for dir in [&good_a, &broken, &good_b] {
        std::fs::create_dir_all(dir).expect("建插件目录");
    }

    let validated_a = package_for(
        &good_a,
        "good-a",
        &["echo"],
        &[],
        &probe_script(&["echo"], 50, &[]),
    );
    // 入口一起进程就退出：失败点在「起进程之后」，不是清单校验阶段。
    let validated_broken = package_for(&broken, "broken", &["echo"], &[], &crashing_script());
    let validated_b = package_for(
        &good_b,
        "good-b",
        &["echo"],
        &[],
        &probe_script(&["echo"], 50, &[]),
    );

    let host = Host::new();
    let (a, bad, b) = tokio::join!(
        host.load(spec_for(&good_a, "good-a", &validated_a, &["fs.read"])),
        host.load(spec_for(&broken, "broken", &validated_broken, &["fs.read"])),
        host.load(spec_for(&good_b, "good-b", &validated_b, &["fs.read"])),
    );

    assert!(a.is_ok(), "坏插件的失败不能传染给 good-a：{:?}", a.err());
    assert!(b.is_ok(), "坏插件的失败不能传染给 good-b：{:?}", b.err());
    assert!(
        matches!(
            bad,
            Err(LoadError::Crashed { .. }) | Err(LoadError::Timeout { .. })
        ),
        "坏插件应如实报装载失败，实际 {bad:?}"
    );

    let mut running = host.running_ids();
    running.sort();
    assert_eq!(running, vec!["good-a".to_string(), "good-b".to_string()]);
    assert!(!host.is_running("broken"), "失败的插件不应留在装载表里");
    assert_eq!(
        host.running_tools("good-a"),
        vec!["plugin__good-a__echo".to_string()]
    );
    assert!(host.running_tools("broken").is_empty());

    host.shutdown_all().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn state_broadcast_reaches_only_subscribing_plugin() {
    if !node_ready() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let root = scratch("broadcast");
    let listener = root.join("listener");
    let bystander = root.join("bystander");
    for dir in [&listener, &bystander] {
        std::fs::create_dir_all(dir).expect("建插件目录");
    }

    let validated_listener = package_for(
        &listener,
        "listener",
        &["echo"],
        &["plugin.state_changed"],
        &probe_script(&["echo"], 20, &["plugin.state_changed"]),
    );
    let validated_bystander = package_for(
        &bystander,
        "bystander",
        &["echo"],
        &[],
        &probe_script(&["echo"], 20, &[]),
    );

    let host = Host::new();
    host.load(spec_for(
        &listener,
        "listener",
        &validated_listener,
        &["fs.read"],
    ))
    .await
    .expect("listener 应装载成功");
    host.load(spec_for(
        &bystander,
        "bystander",
        &validated_bystander,
        &["fs.read"],
    ))
    .await
    .expect("bystander 应装载成功");

    // 广播的是 bystander 的状态迁移：只有订阅了这个事件的 listener 该收到。
    let delivered = host.announce_state("bystander", "ready");
    assert_eq!(delivered, 1, "只应投递给订阅了该事件的那一个插件");

    // 事件是异步写出到插件 stdin 的，给它一点时间落盘。
    let events_file = listener.join("events.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if events_file.exists() && !read_lines(&events_file).is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "listener 一直没收到状态广播");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let received = read_lines(&events_file);
    assert_eq!(
        received.len(),
        1,
        "listener 只应收到本插件那一条：{received:?}"
    );
    assert!(
        received[0].contains("plugin.state_changed") && received[0].contains("bystander"),
        "广播内容应是 bystander 的状态迁移，实际 {}",
        received[0]
    );
    assert!(
        !bystander.join("events.log").exists(),
        "没订阅的插件不该收到任何事件"
    );
    assert_eq!(
        host.pending_events("bystander"),
        0,
        "未订阅的插件队列应保持为空"
    );

    host.shutdown_all().await;
    let _ = std::fs::remove_dir_all(&root);
}
