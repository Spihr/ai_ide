//! 宿主装载/握手的端到端行为（真起 node 子进程，不 mock 协议）。
//!
//! 这里只验插件 crate 自己的承诺：授权不齐不起进程、握手不合规不留半装载状态、
//! 崩溃即下架、卸载即关停、重复 load 幂等。
//! 内核账本与 61xx 映射由 `crates/core/tests/plugin_flow.rs` 负责。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use ai_ide_plugin::{
    host::{Handshake, Host, LoadError, LoadSpec},
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
    let tool_list = tools
        .iter()
        .map(|name| {
            format!(
                "{{ name: {name:?}, description: \"{name} 回显\", risk: 'read', parameters: {{ type: 'object' }} }}"
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let die = die_after_ms
        .map(|ms| format!("setTimeout(() => process.exit(7), {ms});"))
        .unwrap_or_default();
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
