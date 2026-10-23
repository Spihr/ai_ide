//! `plugin.*` 端到端测试（插件系统 P1：清单与安装）。
//!
//! 与其它 `*_flow` 同一套路：起真实内核子进程，只走 stdin / stdout。
//! P1 的内核侧动作全是磁盘和账本，进程内单测能把校验规则测干净，却测不到
//! 「协议参数真的被翻译成了安装动作、账本真的落到了那个数据目录里」。
//!
//! 这组测试要钉住四件事：
//!
//! 1. **非法清单 6107 + `details[]` 一次报全**——六类错误各一条，不是挤牙膏；
//! 2. **安装后 `list` / `status` 立刻可见**，`uninstall` 后立刻消失；
//! 3. **安装全程不产生子进程**——P1 的验收原话是「安装不产生进程」，
//!    所以这里直接数内核的子进程数，而不是看状态字段（状态可能是假的，进程不会）；
//! 4. 仍属 P2/P3 的六个方法继续回 `6101`，且插件错误不许落进终端保留段 `6001`–`6004`。

mod common;

use std::path::{Path, PathBuf};

use common::{Core, TempDataDir};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 写一个插件包目录：`plugin.json` + 入口文件，返回包根。
fn write_package(root: &Path, name: &str, manifest: &Value, entry: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("建包目录");
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(manifest).expect("序列化清单"),
    )
    .expect("写清单");
    // 入口文件真实存在：P1 的清单校验要求 main 指向包内已有文件，
    // 但它**不该被执行**——执行与否由「有没有子进程」那条用例负责证明。
    std::fs::write(dir.join(entry), "// 测试入口：P1 绝不执行这里\n").expect("写入口");
    dir
}

/// 一份合法清单（可按需改字段）。
fn legal_manifest(id: &str, version: &str) -> Value {
    json!({
        "id": id,
        "name": "演示插件",
        "version": version,
        "description": "端到端测试用插件",
        "pluginApi": "1.0",
        "main": "index.mjs",
        "capabilities": [],
        "tools": [
            {
                "name": "echo",
                "description": "原样回显",
                "risk": "read",
                "parameters": {
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }
            }
        ],
        "commands": [
            { "name": "echo_demo", "title": "演示回显" }
        ],
        "subscriptions": []
    })
}

/// 六类非法：id 格式、空 name、semver、main 越界、能力不在白名单、risk 与能力矛盾。
fn broken_manifest() -> Value {
    json!({
        "id": "Demo_Bad",
        "name": "",
        "version": "1.0",
        "pluginApi": "1.0",
        "main": "../../../evil.mjs",
        "capabilities": ["fs.read", "shell.exec"],
        "tools": [
            {
                "name": "evil_write",
                "description": "越权写盘",
                "risk": "write",
                "parameters": { "type": "object", "properties": {} }
            }
        ],
        "commands": []
    })
}

/// 只写清单、不写入口文件的包（`main` 故意指向包外，入口本就不该存在）。
fn write_broken_package(root: &Path, name: &str, manifest: &Value) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("建包目录");
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(manifest).expect("序列化清单"),
    )
    .expect("写清单");
    dir
}

// ---------------------------------------------------------------------------
// P0 遗留：协议面
// ---------------------------------------------------------------------------

/// 十个方法里，只有 4 个在 P1 接了业务；其余 6 个的最小合法参数。
fn unwired_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("plugin.load", json!({ "plugin_id": "demo" })),
        ("plugin.unload", json!({ "plugin_id": "demo" })),
        ("plugin.reload", json!({ "plugin_id": "demo" })),
        (
            "plugin.call",
            json!({ "plugin_id": "demo", "method": "echo" }),
        ),
        (
            "plugin.grant",
            json!({ "plugin_id": "demo", "capabilities": ["fs.read"] }),
        ),
        (
            "plugin.revoke",
            json!({ "plugin_id": "demo", "capabilities": ["fs.read"] }),
        ),
    ]
}

#[test]
fn capabilities_advertise_all_ten_plugin_methods() {
    let data = TempDataDir::new("plugin-caps");
    let mut core = Core::spawn(&data.root);

    let capabilities = core.capabilities();
    let plugin_methods: Vec<&String> = capabilities
        .iter()
        .filter(|name| name.starts_with("plugin."))
        .collect();

    assert_eq!(
        plugin_methods.len(),
        10,
        "能力清单里 plugin.* 应有 10 项，实际 {plugin_methods:?}"
    );
}

#[test]
fn unwired_methods_still_report_6101() {
    let data = TempDataDir::new("plugin-unwired");
    let mut core = Core::spawn(&data.root);

    for (method, params) in unwired_calls() {
        let code = core.error_code(method, params);
        assert_eq!(
            code, 6101,
            "{method} 尚未接线，应回 6101；实际 {code}（-32601 说明 dispatch 分支漏了）"
        );
    }

    // 未知插件：status / uninstall 走真实现后仍是 6101。
    for method in ["plugin.status", "plugin.uninstall"] {
        let code = core.error_code(method, json!({ "plugin_id": "nope" }));
        assert_eq!(code, 6101, "{method} 对不存在的插件应回 6101，实际 {code}");
    }
}

#[test]
fn plugin_errors_never_collide_with_terminal_band() {
    let data = TempDataDir::new("plugin-band");
    let mut core = Core::spawn(&data.root);

    let mut calls = unwired_calls();
    calls.push(("plugin.status", json!({ "plugin_id": "demo" })));
    calls.push((
        "plugin.install",
        json!({ "path": "C:/definitely/not/a/plugin" }),
    ));

    for (method, params) in calls {
        let code = core.error_code(method, params);
        assert!(
            !(6001..=6004).contains(&code),
            "{method} 回了 {code}，那是终端保留段（6001–6004），终端侧会误判成会话错误"
        );
    }
}

#[test]
fn plugin_methods_still_reject_malformed_params_with_invalid_params() {
    let data = TempDataDir::new("plugin-params");
    let mut core = Core::spawn(&data.root);

    for method in [
        "plugin.status",
        "plugin.uninstall",
        "plugin.load",
        "plugin.unload",
        "plugin.reload",
    ] {
        let code = core.error_code(method, json!({}));
        assert_eq!(
            code, -32602,
            "{method} 缺 plugin_id 应回 -32602（参数非法），实际 {code}"
        );
    }

    let code = core.error_code("plugin.call", json!({ "plugin_id": "demo" }));
    assert_eq!(
        code, -32602,
        "plugin.call 缺 method 应回 -32602，实际 {code}"
    );

    // 装了真实现之后，install 也不能因为「path 是空串」就退化成 6101/6107。
    let code = core.error_code("plugin.install", json!({ "path": "   " }));
    assert_eq!(
        code, -32602,
        "plugin.install 空 path 应回 -32602，实际 {code}"
    );
}

// ---------------------------------------------------------------------------
// P1：非法清单
// ---------------------------------------------------------------------------

#[test]
fn invalid_manifest_reports_6107_with_every_detail_in_one_pass() {
    let data = TempDataDir::new("plugin-invalid");
    let packages = TempDataDir::new("plugin-invalid-pkg");
    let mut core = Core::spawn(&data.root);

    let package = write_broken_package(&packages.root, "broken", &broken_manifest());
    let response = core.call(
        "plugin.install",
        json!({ "path": package.to_string_lossy() }),
    );
    let error = response.get("error").expect("非法清单必须报错");

    assert_eq!(
        error["code"],
        json!(6107),
        "顶层码应是 6107（清单非法），实际 {error}"
    );
    let details = error["data"]["details"]
        .as_array()
        .unwrap_or_else(|| panic!("6107 必须带 data.details 数组：{error}"));
    assert_eq!(
        details.len(),
        6,
        "六类错误必须一次报全（不是遇到第一条就返回）：{details:?}"
    );

    let paths: Vec<&str> = details
        .iter()
        .map(|detail| detail["path"].as_str().unwrap_or_default())
        .collect();
    for expected in [
        "$.id",
        "$.name",
        "$.version",
        "$.main",
        "$.capabilities[1]",
        "$.tools[0].risk",
    ] {
        assert!(
            paths.contains(&expected),
            "缺少 {expected} 的报错；实际 {paths:?}"
        );
    }
    // 每条都带 code/path/message 三件套：外壳要原样渲染，不能靠拼字符串。
    for detail in details {
        assert!(
            detail["code"].as_i64().is_some(),
            "detail 缺 code：{detail}"
        );
        assert!(
            detail["message"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "detail 缺 message：{detail}"
        );
    }

    // 被拒的包不该在数据目录里留下任何痕迹。
    assert!(
        !data.root.join("plugins").join("Demo_Bad").exists(),
        "校验失败不该创建安装目录"
    );
    assert!(
        core.ok("plugin.list", json!({}))["plugins"]
            .as_array()
            .expect("plugins 是数组")
            .is_empty(),
        "校验失败不该写账本"
    );
}

// ---------------------------------------------------------------------------
// P1：安装 → 列举 → 详情 → 卸载
// ---------------------------------------------------------------------------

#[test]
fn install_list_status_and_uninstall_round_trip() {
    let data = TempDataDir::new("plugin-round-trip");
    let packages = TempDataDir::new("plugin-round-trip-pkg");
    let mut core = Core::spawn(&data.root);

    let package = write_package(
        &packages.root,
        "demo",
        &legal_manifest("demo", "1.0.0"),
        "index.mjs",
    );

    let installed = core.ok(
        "plugin.install",
        json!({ "path": package.to_string_lossy() }),
    );
    assert_eq!(installed["replaced"], json!(false), "首次安装不该是覆盖");
    assert_eq!(installed["plugin"]["id"], json!("demo"));
    assert_eq!(installed["plugin"]["version"], json!("1.0.0"));
    assert_eq!(
        installed["plugin"]["state"],
        json!("disabled"),
        "P1 装完就是「装好未加载」，绝不是 ready"
    );
    assert_eq!(
        installed["plugin"]["tool_count"],
        json!(1),
        "工具计数来自清单：{installed}"
    );

    // 包真的被复制进了数据目录，而不是只在账本里留了个路径。
    let install_dir = data.root.join("plugins").join("demo");
    assert!(install_dir.join("plugin.json").exists(), "清单未落盘");
    assert!(install_dir.join("index.mjs").exists(), "入口未落盘");

    let listed = core.ok("plugin.list", json!({}));
    let plugins = listed["plugins"].as_array().expect("plugins 是数组");
    assert_eq!(plugins.len(), 1, "列表应只有刚装的一个：{listed}");
    assert_eq!(plugins[0]["id"], json!("demo"));

    let detail = core.ok("plugin.status", json!({ "plugin_id": "demo" }));
    assert_eq!(detail["plugin"]["state"], json!("disabled"));
    let tools = detail["tools"].as_array().expect("tools 是数组");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0]["namespaced_name"],
        json!("plugin__demo__echo"),
        "工具名必须带 plugin__<id>__ 前缀"
    );
    assert_eq!(tools[0]["risk"], json!("read"));
    assert_eq!(
        detail["commands"][0]["title"],
        json!("演示回显"),
        "命令面板入口应透出清单里的 title"
    );

    let removed = core.ok("plugin.uninstall", json!({ "plugin_id": "demo" }));
    assert_eq!(removed["plugin_id"], json!("demo"));
    assert_eq!(removed["removed_dir"], json!(true), "目录应被删掉");
    assert_eq!(
        removed["removed_tools"],
        json!(0),
        "P1 从不注册工具，下架数恒为 0"
    );
    assert!(!install_dir.exists(), "卸载后安装目录应消失");

    assert!(
        core.ok("plugin.list", json!({}))["plugins"]
            .as_array()
            .expect("plugins 是数组")
            .is_empty(),
        "卸载后列表应清空"
    );
    let code = core.error_code("plugin.status", json!({ "plugin_id": "demo" }));
    assert_eq!(code, 6101, "卸载后再查应回 6101，实际 {code}");
}

#[test]
fn upgrade_with_new_capability_flips_state_to_needs_grant() {
    let data = TempDataDir::new("plugin-upgrade");
    let packages = TempDataDir::new("plugin-upgrade-pkg");
    let mut core = Core::spawn(&data.root);

    let v1 = write_package(
        &packages.root,
        "demo-v1",
        &legal_manifest("demo", "1.0.0"),
        "index.mjs",
    );
    let first = core.ok("plugin.install", json!({ "path": v1.to_string_lossy() }));
    assert_eq!(first["plugin"]["state"], json!("disabled"));

    // v1.1 偷偷加 `fs.read`：§5.4 要求状态回退到 needs_grant 重新征询。
    let mut manifest = legal_manifest("demo", "1.1.0");
    manifest["capabilities"] = json!(["fs.read"]);
    let v2 = write_package(&packages.root, "demo-v1-1", &manifest, "index.mjs");
    let second = core.ok("plugin.install", json!({ "path": v2.to_string_lossy() }));

    assert_eq!(second["replaced"], json!(true), "同 id 应是覆盖安装");
    assert_eq!(second["plugin"]["version"], json!("1.1.0"));
    assert_eq!(
        second["plugin"]["state"],
        json!("needs_grant"),
        "新增能力必须退回待授权：{second}"
    );
    assert_eq!(second["plugin"]["capabilities"], json!(["fs.read"]));
    assert_eq!(
        second["plugin"]["granted_capabilities"],
        json!([]),
        "P1 没有任何授权入口，新能力一律未授予"
    );

    let detail = core.ok("plugin.status", json!({ "plugin_id": "demo" }));
    assert_eq!(
        detail["plugin"]["version"],
        json!("1.1.0"),
        "账本应只剩新版本"
    );
}

// ---------------------------------------------------------------------------
// P1：安装不产生进程
// ---------------------------------------------------------------------------

/// Windows：数一数指定进程的直接子进程个数。
#[cfg(windows)]
fn child_process_count(pid: u32) -> usize {
    let script = format!("@(Get-CimInstance Win32_Process -Filter 'ParentProcessId={pid}').Count");
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("查询子进程失败");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

#[cfg(windows)]
#[test]
fn install_never_spawns_a_child_process() {
    let data = TempDataDir::new("plugin-noexec");
    let packages = TempDataDir::new("plugin-noexec-pkg");
    let mut core = Core::spawn(&data.root);

    let before = child_process_count(core.pid());
    let package = write_package(
        &packages.root,
        "demo",
        &legal_manifest("demo", "1.0.0"),
        "index.mjs",
    );
    let installed = core.ok(
        "plugin.install",
        json!({ "path": package.to_string_lossy() }),
    );
    let after = child_process_count(core.pid());

    assert_eq!(
        after, before,
        "安装不得产生子进程（装前 {before}，装后 {after}）——P1 只碰磁盘"
    );
    assert_ne!(
        installed["plugin"]["state"],
        json!("ready"),
        "装完不该是 ready：那意味着已经加载了"
    );
}
