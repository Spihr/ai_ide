//! `lsp.*` 端到端测试（LSP 切片二：dispatch 接线 + 诊断转发）。
//!
//! 与其它 `*_flow` 同一套路：起真实内核子进程，只走 stdin / stdout；语言服务器
//! 用 `crates/lsp` 里的 `lsp-mock-server`——它能在没有 `rust-analyzer` /
//! `typescript-language-server` 的机器上跑，而且「要求文档先 didOpen 才回结果」
//! 这类行为是刻意的，能立刻暴露「客户端漏了同步」。
//!
//! 这组测试要钉住四件事：
//!
//! 1. **能力表如实**：`core.hello` 报出 `lsp.*` 五项，总数 79（74 → 79）；
//! 2. **`lsp.start` → `lsp.request` 能真查到东西**：跳转位置带着 1-based 坐标回来，
//!    服务器给的坏条目（缺 `range`）被丢掉而不是编一个 `0:0`；
//! 3. **诊断是推送**：`publishDiagnostics` 不是任何请求的返回值，必须变成
//!    `lsp.diagnostics` 通知推给外壳；
//! 4. **两类用户可处置的错误各归各码**：句柄不认识 `10003`、语言不认识 `10002`。
//!
//! 为什么只能端到端测：管理器是进程级单例（`lsp_api::manager()`），进程内单测
//! 之间会互相污染连接表；而「参数被翻译成了真正的 LSP 帧」这件事，只有走一遍
//! 真实子进程的 stdio 才能证明。

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::{unique_temp_dir, Core, TempDataDir};
use serde_json::{json, Value};

/// 假服务器里「打开即推诊断」的标记，与 `crates/lsp/src/bin/lsp_mock_server.rs`
/// 的 `DIAGNOSTICS_MARKER` 必须一致（那边是 bin，core 的测试引用不到它的常量）。
const DIAGNOSTICS_MARKER: &str = "// mock:publish-diagnostics";

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 假服务器二进制。
///
/// 内核的测试进程拿不到 `CARGO_BIN_EXE_lsp-mock-server`（那是 `ai-ide-lsp` 的
/// 环境变量），只能按 cargo 的布局从自己所在的 `target/<profile>/deps/` 往上找一层。
/// `cargo test --workspace` 会构建全部成员的 bin，所以它就在那里。
fn mock_server() -> &'static Path {
    static RESOLVED: OnceLock<PathBuf> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        if let Some(path) = std::env::var_os("AI_IDE_LSP_MOCK_SERVER") {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "AI_IDE_LSP_MOCK_SERVER 指向的不是文件：{path:?}"
            );
            return path;
        }
        let exe = std::env::current_exe().expect("测试进程自身路径");
        // `target/<profile>/deps/lsp_flow-<hash>.exe` → `target/<profile>`
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("测试可执行文件应在 target/<profile>/deps 下");
        let candidate =
            profile_dir.join(format!("lsp-mock-server{}", std::env::consts::EXE_SUFFIX));
        assert!(
            candidate.is_file(),
            "找不到假语言服务器 {candidate:?}：先跑一次 `cargo build --workspace`，\
             或用 AI_IDE_LSP_MOCK_SERVER 指定路径"
        );
        candidate
    })
}

/// 一个含指定文件的工作区根（用完即删）。
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(tag: &str) -> Self {
        Self {
            root: unique_temp_dir("ai-ide-lsp", tag),
        }
    }

    /// 写一个文件并返回它的绝对路径。
    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, text).expect("写测试文件");
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 路径比较口径：Windows 反斜杠 / 大小写都不算差异。
fn normalize(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

/// `lsp.start` 的参数：指定 mock 服务器，其余走注册表默认。
fn start_params(root: &Path, path: &Path) -> Value {
    json!({
        "path": display_path(path),
        "root": display_path(root),
        "command": display_path(mock_server()),
    })
}

/// 起一条连接并回 `server_id`。
fn start_server(core: &mut Core, root: &Path, path: &Path) -> String {
    let started = core.ok("lsp.start", start_params(root, path));
    assert_eq!(
        started["reused"],
        json!(false),
        "首次启动不该是复用：{started}"
    );
    let server_id = started["server"]["server_id"]
        .as_str()
        .expect("server_id 必须存在")
        .to_string();
    assert_eq!(
        started["server"]["status"],
        json!("ready"),
        "握手完成后状态必须是 ready：{started}"
    );
    server_id
}

// ---------------------------------------------------------------------------
// 能力表
// ---------------------------------------------------------------------------

#[test]
fn capabilities_advertise_all_five_lsp_methods() {
    let data = TempDataDir::new("lsp-caps");
    let mut core = Core::spawn(&data.root);

    let capabilities = core.capabilities();
    let mut lsp_methods: Vec<&String> = capabilities
        .iter()
        .filter(|name| name.starts_with("lsp."))
        .collect();
    lsp_methods.sort();

    // 顺序按字典序：servers < start 之类的差异不必在这里体现，齐不齐才是重点。
    assert_eq!(
        lsp_methods,
        vec![
            "lsp.request",
            "lsp.servers",
            "lsp.start",
            "lsp.stop",
            "lsp.sync"
        ],
        "能力清单里的 lsp.* 五项对不上：{lsp_methods:?}"
    );
    // 74 → 79：五个方法从「声明」挪进「已实现」，`core.hello` 只能在后者里报。
    assert_eq!(
        capabilities.len(),
        79,
        "能力总数应为 79（74 + lsp 五项），实际 {}",
        capabilities.len()
    );
}

// ---------------------------------------------------------------------------
// 连接生命周期与查询
// ---------------------------------------------------------------------------

#[test]
fn definition_query_returns_locations_from_a_live_server() {
    let data = TempDataDir::new("lsp-definition");
    let workspace = Workspace::new("definition");
    let file = workspace.write("main.ts", "const value = 1;\nconsole.log(value);\n");
    let mut core = Core::spawn(&data.root);

    let server_id = start_server(&mut core, &workspace.root, &file);

    // 同一份文件再 start 一次：必须复用，而不是又起一个进程。
    let again = core.ok("lsp.start", start_params(&workspace.root, &file));
    assert_eq!(
        again["reused"],
        json!(true),
        "同 root / 同语言必须复用：{again}"
    );
    assert_eq!(again["server"]["server_id"], json!(server_id));

    let servers = core.ok("lsp.servers", json!({}));
    let listed = servers["servers"].as_array().expect("servers 是数组");
    assert_eq!(listed.len(), 1, "只起了一条连接：{servers}");
    assert_eq!(listed[0]["server_id"], json!(server_id));

    // 位置是 1-based：`(1, 1)` 到线格式是 `(0, 0)`，mock 回 `line + 2`，
    // 因此回来应是第 3 行。
    let result = core.ok(
        "lsp.request",
        json!({
            "server_id": server_id,
            "method": "definition",
            "path": display_path(&file),
            "position": { "line": 1, "character": 1 },
        }),
    );

    let locations = result["locations"].as_array().expect("locations 是数组");
    assert_eq!(
        locations.len(),
        2,
        "mock 回了 3 条，其中一条缺 range 必须被丢掉：{result}"
    );
    assert_eq!(
        normalize(locations[0]["path"].as_str().expect("path")),
        normalize(&display_path(&file))
    );
    assert_eq!(locations[0]["range"]["start"]["line"], json!(3));
    assert_eq!(locations[0]["range"]["start"]["character"], json!(1));
    // 第二条是 LocationLink 形态（只有 target* 字段），也要能折成可跳转位置；
    // uri 里带百分号编码的中文，落回路径时不能留 `%E4%B8%AD`。
    assert!(
        normalize(locations[1]["path"].as_str().expect("path")).ends_with("中文.ts"),
        "LocationLink 的 uri 未正确解码：{result}"
    );

    // `hover` 走同一个入口：内容被拼成一段文本，不做二次解析。
    let hover = core.ok(
        "lsp.request",
        json!({
            "server_id": server_id,
            "method": "hover",
            "path": display_path(&file),
            "position": { "line": 1, "character": 1 },
        }),
    );
    assert!(
        hover["hover"]["contents"]
            .as_str()
            .expect("hover 必须有内容")
            .contains("const value: number"),
        "hover 内容丢了：{hover}"
    );

    let stopped = core.ok("lsp.stop", json!({ "server_id": server_id }));
    assert_eq!(stopped["stopped"], json!(true));
    assert_eq!(stopped["server_id"], json!(server_id));
}

// ---------------------------------------------------------------------------
// 诊断推送
// ---------------------------------------------------------------------------

#[test]
fn publish_diagnostics_is_forwarded_as_an_lsp_event() {
    let data = TempDataDir::new("lsp-diagnostics");
    let workspace = Workspace::new("diagnostics");
    // 文本里带标记：mock 收到 didOpen 就推一条诊断。这里是唯一能触发推送的入口
    // ——`lsp.request` 只暴露四类查询，没有「让服务器推诊断」的方法。
    let file = workspace.write(
        "app.ts",
        &format!("{DIAGNOSTICS_MARKER}\nexport const broken = 1;\n"),
    );
    let mut core = Core::spawn(&data.root);

    let server_id = start_server(&mut core, &workspace.root, &file);

    // 不带 content：内核从磁盘读，`didOpen` 随之发出，诊断紧跟着被推上来。
    let synced = core.ok(
        "lsp.sync",
        json!({
            "server_id": server_id,
            "path": display_path(&file),
        }),
    );
    assert_eq!(
        synced["opened"],
        json!(true),
        "首次同步应 didOpen：{synced}"
    );
    assert_eq!(synced["version"], json!(1));

    let event = core.wait_notification("lsp.diagnostics");
    let params = event.get("params").expect("诊断事件必须带 params");
    assert_eq!(params["server_id"], json!(server_id));
    assert_eq!(
        normalize(params["path"].as_str().expect("path")),
        normalize(&display_path(&file)),
        "诊断必须落回本地路径：{params}"
    );
    assert_eq!(params["version"], json!(1));

    let diagnostics = params["diagnostics"].as_array().expect("诊断数组");
    assert_eq!(diagnostics.len(), 1, "{params}");
    let diagnostic = &diagnostics[0];
    // LSP 线格式是数字 2；协议面必须是 `warning`，不是原样透传数字。
    assert_eq!(diagnostic["severity"], json!("warning"), "{params}");
    assert_eq!(diagnostic["message"], json!("打开文档时推送的诊断"));
    assert_eq!(diagnostic["code"], json!("MOCK1001"));
    assert_eq!(diagnostic["source"], json!("lsp-mock-server"));
    // 坐标同样是 1-based：线格式 `0:0` → `1:1`。
    assert_eq!(
        diagnostic["range"]["start"],
        json!({ "line": 1, "character": 1 })
    );

    core.ok("lsp.stop", json!({ "server_id": server_id }));
}

// ---------------------------------------------------------------------------
// 错误码
// ---------------------------------------------------------------------------

#[test]
fn request_for_an_unknown_or_stopped_server_is_10003() {
    let data = TempDataDir::new("lsp-unknown");
    let workspace = Workspace::new("unknown");
    let file = workspace.write("main.ts", "const value = 1;\n");
    let mut core = Core::spawn(&data.root);

    // 从未启动过：`10003` 让外壳提示「重新 lsp.start」，而不是「服务器没装」。
    let code = core.error_code(
        "lsp.request",
        json!({
            "server_id": "never-started",
            "method": "definition",
            "path": display_path(&file),
            "position": { "line": 1, "character": 1 },
        }),
    );
    assert_eq!(
        code, 10003,
        "未知句柄必须是 10003（-32601 说明 dispatch 没接线）"
    );

    // 关掉之后再问同一个句柄，还是 10003：表里已经没有它了。
    let server_id = start_server(&mut core, &workspace.root, &file);
    core.ok("lsp.stop", json!({ "server_id": server_id }));

    for (method, params) in [
        (
            "lsp.request",
            json!({
                "server_id": server_id,
                "method": "hover",
                "path": display_path(&file),
                "position": { "line": 1, "character": 1 },
            }),
        ),
        (
            "lsp.sync",
            json!({ "server_id": server_id, "path": display_path(&file) }),
        ),
        ("lsp.stop", json!({ "server_id": server_id })),
    ] {
        let code = core.error_code(method, params);
        assert_eq!(
            code, 10003,
            "{method} 对已关闭的句柄应回 10003，实际 {code}"
        );
    }
}

#[test]
fn unknown_language_is_10002() {
    let data = TempDataDir::new("lsp-language");
    let workspace = Workspace::new("language");
    // 注册表里没有这个扩展名，也没显式给 language_id。
    let file = workspace.write("notes.unknownext", "hello\n");
    let mut core = Core::spawn(&data.root);

    let code = core.error_code(
        "lsp.start",
        json!({
            "path": display_path(&file),
            "root": display_path(&workspace.root),
            "command": display_path(mock_server()),
        }),
    );
    assert_eq!(
        code, 10002,
        "推断不出语言必须是 10002（10001 会把用户指去装服务器）"
    );
}
