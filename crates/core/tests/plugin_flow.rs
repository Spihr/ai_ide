//! `plugin.*` 协议骨架的端到端测试（插件系统 P0）。
//!
//! P0 只交付「协议面 + dispatch 分支」：十个方法都能路由，但业务一律未接线，
//! 所以每个方法都应返回 `6101`（`PLUGIN_NOT_FOUND`）。这组测试要挡住的失败是
//! **「常量加了、分支没加」**——那种情况下外壳拿到的是 `-32601`，看起来只是
//! 「方法名敲错了」，插件面板会永远猜不出自己该显示「未启用」还是「你写错了」。
//!
//! 因此这里逐个方法问一遍真实内核子进程，并额外钉三件事：
//! 1. `6001`–`6004` 是终端的，插件绝不能回这几个码（撞码会让终端的错误处理
//!    把插件错误当成「会话不存在」）；
//! 2. 缺 `plugin_id` 必须回 `-32602` 而不是 `6101`，否则畸形请求会得到
//!    「插件不存在」的误导性答复；
//! 3. 能力清单里十项齐全——`core.hello` 是外壳唯一的可信来源。

mod common;

use common::{Core, TempDataDir};
use serde_json::{json, Value};

/// 十个 P0 方法及其最小合法参数。
fn p0_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("plugin.list", json!({})),
        ("plugin.status", json!({ "plugin_id": "demo" })),
        ("plugin.install", json!({ "path": "C:/tmp/demo-plugin" })),
        ("plugin.uninstall", json!({ "plugin_id": "demo" })),
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

    for (method, _) in p0_calls() {
        assert!(
            capabilities.iter().any(|name| name == method),
            "能力清单缺少 {method}：{capabilities:?}"
        );
    }
}

#[test]
fn every_plugin_method_routes_to_6101_not_method_not_found() {
    let data = TempDataDir::new("plugin-routing");
    let mut core = Core::spawn(&data.root);

    for (method, params) in p0_calls() {
        let code = core.error_code(method, params);
        assert_eq!(
            code, 6101,
            "{method} 应回 6101（插件未加载），实际 {code}；-32601 说明 dispatch 分支漏了"
        );
    }
}

#[test]
fn plugin_errors_never_collide_with_terminal_band() {
    let data = TempDataDir::new("plugin-band");
    let mut core = Core::spawn(&data.root);

    for (method, params) in p0_calls() {
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
        // 缺 plugin_id：必须报参数错，而不是「插件不存在」。
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
}
