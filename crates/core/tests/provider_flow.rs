//! M2 端到端流程测试：Provider / 模型目录走真实子进程的 stdin / stdout 协议。
//!
//! 与 `workspace_flow.rs` 同样的理由——M2 的风险集中在「线协议编解码 + 分发 +
//! 落盘 + 脱敏 + 事件推送」这条链路上，只测 `provider_api` 里的函数会漏掉整层。
//! 进程脚手架在 `tests/common/mod.rs`，本文件只放断言。
//!
//! **全程离线**：只覆盖目录读写、密钥脱敏、事件与错误码；不发任何真实 HTTP 请求。
//! 真实协议适配（openai 流式分帧、错误分类）由 `crates/llm/tests/provider_stream.rs`
//! 用传输替身覆盖、`crates/core/tests/llm_flow.rs` 用本地假上游覆盖，合起来才是完整的 M2 证据。
//!
//! 每个测试把内核的数据目录指向自己的临时目录（`AI_IDE_DATA_DIR`），
//! 因此不会污染开发机上真实的 `%APPDATA%\ai-ide`。

use serde_json::{json, Value};

mod common;

use common::{upsert_params, Core, TempDataDir, PLAINTEXT_KEY};

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[test]
fn provider_与模型目录_端到端链路() {
    let data = TempDataDir::new("flow");
    let mut core = Core::spawn(&data.root);

    // 1) 握手：能力清单必须覆盖 M2 的全部十一个方法。少报会让 UI 把可用功能藏起来，
    //    多报会让人照单调用、然后拿一片 -32601——`methods.rs` 的单测守住这条，
    //    这里再确认它真的走到了线上。
    let capabilities = core.capabilities();
    for method in [
        "provider.upsert",
        "provider.list",
        "provider.delete",
        "provider.test",
        "model.upsert",
        "model.list",
        "model.delete",
        "llm.chat_stream",
        "llm.cancel",
        "llm.list_models",
        "llm.count_tokens",
    ] {
        assert!(
            capabilities.contains(&method.to_string()),
            "能力清单缺 {method}：{capabilities:?}"
        );
    }

    // 2) 空目录起步。
    assert_eq!(core.ok("provider.list", json!({}))["providers"], json!([]));

    // 3) 新建：id 由内核生成，密钥只进内存。
    let created = core.ok("provider.upsert", upsert_params(Some(PLAINTEXT_KEY)));
    let provider_id = created["provider_id"]
        .as_str()
        .expect("provider_id 是字符串")
        .to_string();
    assert!(provider_id.starts_with("p-"), "id 形状：{provider_id}");

    // 4) 列表：就地验证脱敏视图。
    let listed = core.ok("provider.list", json!({}));
    let providers = listed["providers"].as_array().expect("providers 是数组");
    assert_eq!(providers.len(), 1);
    let view = &providers[0];
    assert_eq!(view["id"], json!(provider_id));
    assert_eq!(view["name"], json!("本地网关"));
    assert_eq!(view["protocol"], json!("openai_chat_completions"));
    assert_eq!(view["enabled"], json!(true));
    assert_eq!(view["has_key"], json!(true));
    assert_eq!(view["last_test_ok"], json!(null));
    assert_eq!(
        view["headers"]["X-Api-Version"],
        json!("2024-08-01"),
        "附加请求头必须原样保留"
    );
    let hint = view["key_hint"].as_str().expect("key_hint 是字符串");
    assert_eq!(hint, "sk-a…mnop", "指纹只露头尾");

    // 5) 明文密钥不得出现在响应里任何位置。
    assert!(
        !listed.to_string().contains(PLAINTEXT_KEY),
        "provider.list 泄露了明文密钥：{listed}"
    );

    // 6) 明文密钥不得落盘。
    let store_text = data.store_text();
    assert!(store_text.contains(&provider_id), "目录必须落盘");
    assert!(
        !store_text.contains(PLAINTEXT_KEY),
        "providers.json 泄露了明文密钥：{store_text}"
    );

    // 7) 目录变更要推事件，否则设置界面得靠轮询。
    let changed: Vec<&Value> = core
        .events()
        .iter()
        .filter(|event| event["method"] == json!("provider.changed"))
        .collect();
    assert_eq!(
        changed.len(),
        1,
        "一次 upsert 推一条事件：{:?}",
        core.events()
    );
    assert_eq!(changed[0]["params"]["kind"], json!("upserted"));
    assert_eq!(changed[0]["params"]["provider_id"], json!(provider_id));

    // 8) 模型挂到该 Provider 下。
    let model = core.ok(
        "model.upsert",
        json!({
            "provider_id": provider_id,
            "model_id": "deepseek-chat",
            "display_name": "DeepSeek Chat",
            "caps": { "streaming": true, "tools": true, "reasoning": false, "vision": false },
            "params": { "temperature": 0.2, "max_output_tokens": 4096 },
        }),
    );
    let model_pk = model["model_pk"].as_str().expect("model_pk 是字符串");
    assert_eq!(model_pk, format!("{provider_id}:deepseek-chat"));

    let models = core.ok("model.list", json!({ "provider_id": provider_id }));
    let models = models["models"].as_array().expect("models 是数组");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["model_id"], json!("deepseek-chat"));
    assert_eq!(models[0]["display_name"], json!("DeepSeek Chat"));
    assert_eq!(models[0]["caps"]["tools"], json!(true));
    assert_eq!(models[0]["params"]["temperature"], json!(0.2));

    // 9) 删除模型：删完列表为空。
    assert_eq!(
        core.ok("model.delete", json!({ "model_pk": model_pk }))["ok"],
        json!(true)
    );
    assert_eq!(
        core.ok("model.list", json!({}))["models"],
        json!([]),
        "不带过滤条件时应列出全部模型"
    );

    // 10) 删除 Provider：记录、注册表项与内存密钥一起清掉。
    assert_eq!(
        core.ok("provider.delete", json!({ "provider_id": provider_id }))["ok"],
        json!(true)
    );
    assert_eq!(core.ok("provider.list", json!({}))["providers"], json!([]));
    assert!(
        !data.store_text().contains(&provider_id),
        "删除后不应残留记录"
    );

    // 11) 已删除的 Provider 不能再测连通性。
    assert_eq!(
        core.error_code("provider.test", json!({ "provider_id": provider_id })),
        2001,
        "PROVIDER_NOT_CONFIGURED"
    );

    // 12) 已删除的 Provider 上的 `llm.*`：如实报 2001（PROVIDER_NOT_CONFIGURED），
    //     而不是拿一片 -32601 或者假装受理。
    assert_eq!(
        core.error_code(
            "llm.chat_stream",
            json!({
                "stream_id": "s-孤儿",
                "model_ref": { "provider_id": provider_id, "model_id": "deepseek-chat" },
                "messages": [{ "role": "user", "content": "你好" }],
            }),
        ),
        2001
    );
    assert_eq!(
        core.error_code("llm.list_models", json!({ "provider_id": provider_id })),
        2001
    );

    // 13) token 估算是纯本地计算，网关没了也得能算——输入框的计数不该随上游一起消失。
    let counted = core.ok(
        "llm.count_tokens",
        json!({
            "model_ref": { "provider_id": provider_id, "model_id": "deepseek-chat" },
            "messages": [{ "role": "user", "content": "你好，世界" }],
        }),
    );
    assert!(counted["tokens"].as_u64().unwrap_or(0) > 0, "{counted}");
    assert_eq!(counted["exact"], json!(false), "M2 只有估算，不能谎报精确");
    assert!(counted["note"].is_string(), "必须带口径说明：{counted}");

    // 14) 取消未命中不算错误：取消请求常与「流刚好自己结束」竞态，
    //     此时报错只会变成一条毫无意义的红色提示。
    assert_eq!(
        core.ok("llm.cancel", json!({ "stream_id": "s-从未存在" }))["ok"],
        json!(true)
    );
}

#[test]
fn 目录跨进程保留_密钥不保留() {
    let data = TempDataDir::new("restart");

    let provider_id = {
        let mut core = Core::spawn(&data.root);
        let created = core.ok("provider.upsert", upsert_params(Some(PLAINTEXT_KEY)));
        created["provider_id"]
            .as_str()
            .expect("provider_id")
            .to_string()
        // core 在这里被 drop：进程退出，目录已经落盘。
    };

    // 重启后：Provider 还在，但密钥必须重配——磁盘上从来就没有密钥。
    let mut core = Core::spawn(&data.root);
    let listed = core.ok("provider.list", json!({}));
    let providers = listed["providers"].as_array().expect("providers 是数组");
    assert_eq!(providers.len(), 1, "落盘目录应当跨进程保留");
    assert_eq!(providers[0]["id"], json!(provider_id));
    assert_eq!(providers[0]["has_key"], json!(false));
    assert_eq!(
        providers[0]["key_hint"],
        json!(null),
        "没有密钥就不能给出指纹，否则 UI 会以为配好了"
    );
}

#[test]
fn 非法参数与未接线协议_如实报错() {
    let data = TempDataDir::new("errors");
    let mut core = Core::spawn(&data.root);

    // 1) 带 id 更新一个不存在的 Provider：必须拒绝，而不是静默新增脏记录。
    let mut update = upsert_params(Some(PLAINTEXT_KEY));
    update["id"] = json!("p-一厢情愿");
    assert_eq!(
        core.error_code("provider.upsert", update),
        -32602,
        "INVALID_PARAMS"
    );
    assert_eq!(core.ok("provider.list", json!({}))["providers"], json!([]));

    // 2) 协议尚未接线的 Provider：记录会存下（免得用户白填一遍），但要如实报错。
    assert_eq!(
        core.error_code(
            "provider.upsert",
            json!({
                "name": "Gemini",
                "protocol": "gemini",
                "base_url": "https://generativelanguage.googleapis.com",
            }),
        ),
        2005,
        "PROVIDER_UNAVAILABLE：记录已保存但实例建不出来"
    );
    let listed = core.ok("provider.list", json!({}));
    assert_eq!(
        listed["providers"].as_array().expect("数组").len(),
        1,
        "记录应已保存，供用户改配置"
    );
    assert_eq!(listed["providers"][0]["protocol"], json!("gemini"));

    // 3) 孤儿模型：Provider 不存在时拒绝。
    assert_eq!(
        core.error_code(
            "model.upsert",
            json!({ "provider_id": "p-不存在", "model_id": "ghost" })
        ),
        -32602
    );

    // 4) 删除不存在的模型 / Provider：同样如实报错，而不是回 ok。
    assert_eq!(
        core.error_code("model.delete", json!({ "model_pk": "p-不存在:ghost" })),
        -32602
    );
    assert_eq!(
        core.error_code("provider.delete", json!({ "provider_id": "p-不存在" })),
        -32602
    );

    // 5) 内联探测既没给 provider_id 也没给 override：参数不完整。
    assert_eq!(core.error_code("provider.test", json!({})), -32602);
}
