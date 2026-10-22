//! 持久化层集成测试。
//!
//! 内存库覆盖读写语义（含排序、分页、错误分支），文件库覆盖「进程重开后数据仍在」，
//! 以及迁移本身是否真的落地了表结构。

use ai_ide_protocol::{ChatRole, ChatToolCall, ModelRef};
use ai_ide_store::{NewMessage, NewSession, SessionQuery, Store, StoreError};

fn demo_model_ref() -> ModelRef {
    ModelRef {
        provider_id: "provider-1".to_string(),
        model_id: "model-1".to_string(),
    }
}

#[tokio::test]
async fn migrations_apply_on_open_and_tables_start_empty() {
    let store = Store::open_in_memory().await.expect("打开内存库");

    let applied = store.applied_migrations().await.expect("读取迁移记录");
    assert_eq!(applied.len(), 3, "当前有会话 / 审计 / 权限规则三个迁移版本");
    assert_eq!(applied[0].0, 1);
    assert_eq!(applied[1].0, 2);
    assert_eq!(applied[2].0, 3);

    assert_eq!(store.count_sessions(None, None).await.unwrap(), 0);
    assert_eq!(store.count_messages("whatever").await.unwrap(), 0);
    assert!(store.get_session("whatever").await.unwrap().is_none());

    // 迁移幂等：重复执行不报错、不重复记录。
    store.migrate().await.expect("重复迁移应幂等");
    assert_eq!(store.applied_migrations().await.unwrap().len(), 3);
}

#[tokio::test]
async fn session_crud_round_trip() {
    let store = Store::open_in_memory().await.expect("打开内存库");

    let created = store
        .create_session(
            NewSession::new("会话 A")
                .with_workspace_path("E:/ai_ide")
                .with_model_ref(demo_model_ref())
                .with_created_at(1_700_000_000_000),
        )
        .await
        .expect("创建会话");
    assert_eq!(created.created_at, created.updated_at);
    assert!(!created.id.is_empty(), "id 应由存储层生成");

    let fetched = store
        .get_session(&created.id)
        .await
        .unwrap()
        .expect("会话应存在");
    assert_eq!(fetched.title, "会话 A");
    assert_eq!(fetched.workspace_path.as_deref(), Some("E:/ai_ide"));
    assert_eq!(fetched.model_ref, Some(demo_model_ref()));

    // 列表按 updated_at 倒序：改名后的 A 会排到 B 前面。
    let other = store
        .create_session(NewSession::new("会话 B").with_created_at(1_700_000_002_000))
        .await
        .unwrap();
    let before_rename = store.list_sessions(SessionQuery::default()).await.unwrap();
    assert_eq!(
        ids(&before_rename),
        vec![other.id.clone(), created.id.clone()],
        "未改名前按 updated_at 倒序"
    );

    assert!(store
        .rename_session(&created.id, "会话 A（改名）", 1_700_000_003_000)
        .await
        .unwrap());
    let renamed = store.get_session(&created.id).await.unwrap().unwrap();
    assert_eq!(renamed.title, "会话 A（改名）");
    assert_eq!(renamed.updated_at, 1_700_000_003_000);

    // updated_at 只增不减：传入更早的时间不应把会话顶回末尾。
    assert!(store
        .rename_session(&created.id, "会话 A（改名）", 1_699_999_999_000)
        .await
        .unwrap());
    assert_eq!(
        store
            .get_session(&created.id)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        1_700_000_003_000
    );

    let after_rename = store.list_sessions(SessionQuery::default()).await.unwrap();
    assert_eq!(
        ids(&after_rename),
        vec![created.id.clone(), other.id.clone()]
    );

    // 会话默认模型可改；`updated_at` 与改名同口径（只增不减）。
    let switched = ModelRef {
        provider_id: "provider-2".to_string(),
        model_id: "model-2".to_string(),
    };
    assert!(store
        .set_session_model(&created.id, &switched, 1_700_000_004_000)
        .await
        .unwrap());
    let re_modelled = store.get_session(&created.id).await.unwrap().unwrap();
    assert_eq!(re_modelled.model_ref, Some(switched));
    assert_eq!(re_modelled.updated_at, 1_700_000_004_000);
    assert!(!store
        .set_session_model("missing", &demo_model_ref(), 1_700_000_005_000)
        .await
        .unwrap());

    // 工作区过滤 + 计数 + 分页
    assert_eq!(store.count_sessions(None, None).await.unwrap(), 2);
    assert_eq!(
        store.count_sessions(Some("E:/ai_ide"), None).await.unwrap(),
        1
    );
    let filtered = store
        .list_sessions(SessionQuery::workspace("E:/ai_ide"))
        .await
        .unwrap();
    assert_eq!(ids(&filtered), vec![created.id.clone()]);
    let paged = store
        .list_sessions(SessionQuery::default().with_limit(1).with_offset(1))
        .await
        .unwrap();
    assert_eq!(ids(&paged), vec![other.id.clone()]);

    // 未知 id 改名 / 删除
    assert!(!store.rename_session("missing", "x", 1).await.unwrap());
    assert!(!store.delete_session("missing").await.unwrap());

    assert!(store.delete_session(&created.id).await.unwrap());
    assert!(store.get_session(&created.id).await.unwrap().is_none());
    assert_eq!(store.count_sessions(None, None).await.unwrap(), 1);
}

#[tokio::test]
async fn append_and_read_messages_keeps_order_and_touches_session() {
    let store = Store::open_in_memory().await.expect("打开内存库");
    let session = store
        .create_session(NewSession::new("对话").with_created_at(1_700_000_000_000))
        .await
        .unwrap();

    let base = 1_700_000_100_000;
    let first = store
        .append_message(NewMessage::user(&session.id, "你好").with_created_at(base))
        .await
        .unwrap();
    // 与 first 同毫秒：验证同刻消息仍按插入顺序稳定（rowid 兜底）
    let second = store
        .append_message(
            NewMessage::assistant(&session.id, "你好，需要什么帮助？")
                .with_model_ref(demo_model_ref())
                .with_created_at(base),
        )
        .await
        .unwrap();
    let third = store
        .append_message(
            NewMessage::tool(&session.id, "call_1", r#"{"ok":true}"#).with_created_at(base + 1),
        )
        .await
        .unwrap();

    let listed = store.list_messages(&session.id, 50, 0).await.unwrap();
    assert_eq!(
        listed.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        vec![first.id.clone(), second.id.clone(), third.id.clone()]
    );
    assert_eq!(listed[0].role, ChatRole::User);
    assert_eq!(listed[1].role, ChatRole::Assistant);
    assert_eq!(listed[1].model_ref, Some(demo_model_ref()));
    assert_eq!(listed[2].role, ChatRole::Tool);
    assert_eq!(listed[2].tool_call_id.as_deref(), Some("call_1"));

    // 逐条读回
    let single = store.get_message(&second.id).await.unwrap().unwrap();
    assert_eq!(single.content, "你好，需要什么帮助？");
    assert_eq!(single.session_id, session.id);
    assert_eq!(single.created_at, base);

    assert_eq!(store.count_messages(&session.id).await.unwrap(), 3);

    // 分页
    let page = store.list_messages(&session.id, 1, 1).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, second.id);

    // 最近 N 条：仍是时间正序
    let recent = store.list_recent_messages(&session.id, 2).await.unwrap();
    assert_eq!(
        recent.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        vec![second.id.clone(), third.id.clone()]
    );

    // 追加消息会把会话 updated_at 推进到不早于该消息
    let touched = store.get_session(&session.id).await.unwrap().unwrap();
    assert_eq!(touched.updated_at, base + 1);

    // 转回协议层 DTO
    let dto = listed[1].to_chat_message();
    assert_eq!(dto.role, ChatRole::Assistant);
    assert_eq!(dto.content, "你好，需要什么帮助？");

    // 删除单条
    assert!(store.delete_message(&third.id).await.unwrap());
    assert!(!store.delete_message(&third.id).await.unwrap());
    assert_eq!(store.count_messages(&session.id).await.unwrap(), 2);
}

#[tokio::test]
async fn tool_calls_and_usage_survive_round_trip() {
    let store = Store::open_in_memory().await.unwrap();
    let session = store
        .create_session(NewSession::new("工具调用"))
        .await
        .unwrap();

    let call = ChatToolCall {
        id: "call_42".to_string(),
        name: "read_file".to_string(),
        arguments: r#"{"path":"src/lib.rs"}"#.to_string(),
    };

    let saved = store
        .append_message(
            NewMessage::assistant(&session.id, "")
                .with_tool_calls(vec![call.clone()])
                .with_usage(serde_json::json!({
                    "prompt_tokens": 12,
                    "completion_tokens": 3,
                    "total_tokens": 15,
                })),
        )
        .await
        .unwrap();

    let loaded = store.get_message(&saved.id).await.unwrap().unwrap();
    assert_eq!(loaded.tool_calls, vec![call.clone()]);
    assert_eq!(loaded.content, "");
    assert_eq!(
        loaded.usage.as_ref().unwrap()["total_tokens"],
        serde_json::json!(15)
    );
    assert_eq!(loaded.to_chat_message().tool_calls, vec![call]);
}

#[tokio::test]
async fn message_content_uses_created_at_when_not_specified() {
    let before = ai_ide_store::now_millis();
    let store = Store::open_in_memory().await.unwrap();
    let session = store
        .create_session(NewSession::new("默认时间"))
        .await
        .unwrap();
    let message = store
        .append_message(NewMessage::user(&session.id, "hi"))
        .await
        .unwrap();
    assert!(message.created_at >= before);
    assert_eq!(
        store
            .get_session(&session.id)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        message.created_at
    );
}

#[tokio::test]
async fn appending_to_unknown_session_is_rejected() {
    let store = Store::open_in_memory().await.unwrap();

    let err = store
        .append_message(NewMessage::user("missing-session", "hi"))
        .await
        .expect_err("目标会话不存在时应报错");
    match &err {
        StoreError::SessionNotFound(id) => assert_eq!(id, "missing-session"),
        other => panic!("错误类型应为 SessionNotFound，实际：{other:?}"),
    }
    assert_eq!(store.count_messages("missing-session").await.unwrap(), 0);
}

#[tokio::test]
async fn deleting_session_removes_its_messages() {
    let store = Store::open_in_memory().await.unwrap();
    let session = store.create_session(NewSession::new("待删")).await.unwrap();
    store
        .append_message(NewMessage::user(&session.id, "hi"))
        .await
        .unwrap();
    assert_eq!(store.count_messages(&session.id).await.unwrap(), 1);

    assert!(store.delete_session(&session.id).await.unwrap());
    assert_eq!(store.count_messages(&session.id).await.unwrap(), 0);
}

#[tokio::test]
async fn file_backed_store_survives_reopen() {
    let dir = std::env::temp_dir().join(format!("ai-ide-store-test-{}", uuid::Uuid::new_v4()));
    let db = dir.join("data.sqlite");
    assert!(!dir.exists(), "临时目录名应唯一");

    let (session_id, message_id) = {
        let store = Store::connect(&db).await.expect("打开文件库");
        assert!(dir.exists(), "connect 应自动创建父目录");

        let session = store
            .create_session(NewSession::new("重启后仍在").with_workspace_path("E:/ai_ide"))
            .await
            .unwrap();
        let message = store
            .append_message(NewMessage::user(&session.id, "落盘的消息"))
            .await
            .unwrap();
        store.close().await;
        (session.id, message.id)
    };

    let reopened = Store::connect(&db).await.expect("重新打开文件库");
    assert_eq!(reopened.count_sessions(None, None).await.unwrap(), 1);
    let session = reopened
        .get_session(&session_id)
        .await
        .unwrap()
        .expect("会话应仍在");
    assert_eq!(session.title, "重启后仍在");
    let message = reopened
        .get_message(&message_id)
        .await
        .unwrap()
        .expect("消息应仍在");
    assert_eq!(message.content, "落盘的消息");
    assert_eq!(reopened.count_messages(&session_id).await.unwrap(), 1);
    reopened.close().await;

    // 清理本用例自己创建的临时目录（尽力而为，不影响断言结果）。
    let _ = std::fs::remove_dir_all(&dir);
}

/// 免问规则必须跨进程活着：用户点过一次「总是允许」，重启后不该再问一遍；
/// 而撤销必须同样落盘，否则「永久同意」就成了唯一的终点。
#[tokio::test]
async fn permission_rules_survive_reopen_and_revoke() {
    let dir = std::env::temp_dir().join(format!("ai-ide-perm-test-{}", uuid::Uuid::new_v4()));
    let db = dir.join("data.sqlite");

    {
        let store = Store::connect(&db).await.expect("打开文件库");
        store
            .remember_permission(ai_ide_store::NewPermissionRule::allow(
                "command:cargo",
                "run_command",
                "E:/ai_ide",
            ))
            .await
            .expect("记下免问");
        store
            .remember_permission(ai_ide_store::NewPermissionRule::allow(
                "path:edit:E:/ai_ide/src",
                "edit",
                "E:/ai_ide",
            ))
            .await
            .expect("记下免问");
        store.close().await;
    }

    let reopened = Store::connect(&db).await.expect("重新打开");
    let mut ids = reopened.permission_ids(None).await.expect("读回键");
    ids.sort();
    assert_eq!(ids, vec!["command:cargo", "path:edit:E:/ai_ide/src"]);

    let rules = reopened
        .list_permissions(Some("E:/ai_ide"), 50)
        .await
        .unwrap();
    assert_eq!(rules.len(), 2);
    assert!(rules.iter().all(|rule| rule.policy == "allow"));

    assert!(reopened.revoke_permission("command:cargo").await.unwrap());
    reopened.close().await;

    let reopened = Store::connect(&db).await.expect("再开");
    assert_eq!(
        reopened.permission_ids(None).await.unwrap(),
        vec!["path:edit:E:/ai_ide/src".to_string()],
        "撤销也要落盘"
    );
    reopened.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fork_copies_messages_and_remaps_parents() {
    let store = Store::open_in_memory().await.expect("打开内存库");

    let source = store
        .create_session(
            NewSession::new("主线")
                .with_workspace_path("E:/ai_ide")
                .with_model_ref(demo_model_ref())
                .with_created_at(1_700_000_000_000),
        )
        .await
        .unwrap();
    let first = store
        .append_message(NewMessage::user(&source.id, "第一问").with_created_at(1_700_000_001_000))
        .await
        .unwrap();
    let second = store
        .append_message(
            NewMessage::assistant(&source.id, "第一答")
                .with_parent(&first.id)
                .with_created_at(1_700_000_002_000),
        )
        .await
        .unwrap();
    let third = store
        .append_message(
            NewMessage::user(&source.id, "第二问")
                .with_parent(&second.id)
                .with_created_at(1_700_000_003_000),
        )
        .await
        .unwrap();

    let outcome = store
        .fork_session(&source.id, None, None)
        .await
        .expect("分叉应成功");

    assert_ne!(outcome.session.id, source.id, "新会话必须换 id");
    assert_eq!(
        outcome.session.title, "主线（分支）",
        "未指定标题时给默认后缀"
    );
    assert_eq!(
        outcome.session.workspace_path.as_deref(),
        Some("E:/ai_ide"),
        "工作区要跟着复制"
    );
    assert_eq!(outcome.session.model_ref, Some(demo_model_ref()));
    assert_eq!(outcome.messages.len(), 3);

    // 新会话里的消息 id 必须全部换新，父链在新 id 空间内重新串起来。
    assert!(outcome.messages.iter().all(|m| m.id != first.id));
    assert!(outcome
        .messages
        .iter()
        .all(|m| m.session_id == outcome.session.id));
    assert_eq!(outcome.messages[0].parent_id, None);
    assert_eq!(
        outcome.messages[1].parent_id.as_deref(),
        Some(outcome.messages[0].id.as_str())
    );
    assert_eq!(
        outcome.messages[2].parent_id.as_deref(),
        Some(outcome.messages[1].id.as_str())
    );
    // 历史时间原样保留：fork 是复制不是重写。
    assert_eq!(outcome.messages[2].created_at, 1_700_000_003_000);

    // 源会话毫发无损，且新会话真的落了盘。
    let source_messages = store.list_messages(&source.id, 10, 0).await.unwrap();
    assert_eq!(source_messages.len(), 3);
    assert_eq!(source_messages[0].id, first.id);
    assert_eq!(store.count_messages(&outcome.session.id).await.unwrap(), 3);
    assert_eq!(store.count_sessions(None, None).await.unwrap(), 2);
    let _ = third;
}

#[tokio::test]
async fn fork_truncates_at_from_message_and_orphans_parent() {
    let store = Store::open_in_memory().await.expect("打开内存库");

    let source = store.create_session(NewSession::new("主线")).await.unwrap();
    let first = store
        .append_message(NewMessage::user(&source.id, "第一问"))
        .await
        .unwrap();
    let second = store
        .append_message(NewMessage::assistant(&source.id, "第一答").with_parent(&first.id))
        .await
        .unwrap();
    store
        .append_message(NewMessage::user(&source.id, "第二问").with_parent(&second.id))
        .await
        .unwrap();

    // 截断到第一条：父亲的 id 落在本批之外，新会话里不能保留这个悬空引用。
    let outcome = store
        .fork_session(&source.id, Some(&first.id), Some("只要第一句"))
        .await
        .expect("截断分叉应成功");
    assert_eq!(outcome.session.title, "只要第一句");
    assert_eq!(outcome.messages.len(), 1);
    assert_eq!(outcome.messages[0].content, "第一问");
    assert_eq!(outcome.messages[0].parent_id, None);

    // 截断到第二条：父链已被复制，应完整重映射。
    let outcome = store
        .fork_session(&source.id, Some(&second.id), None)
        .await
        .expect("截断分叉应成功");
    assert_eq!(outcome.messages.len(), 2);
    assert_eq!(
        outcome.messages[1].parent_id.as_deref(),
        Some(outcome.messages[0].id.as_str())
    );
}

#[tokio::test]
async fn fork_rejects_unknown_session_and_foreign_message() {
    let store = Store::open_in_memory().await.expect("打开内存库");

    let missing = store
        .fork_session("missing-session", None, None)
        .await
        .expect_err("源会话不存在应报错");
    assert!(matches!(missing, StoreError::SessionNotFound(id) if id == "missing-session"));

    let source = store.create_session(NewSession::new("源")).await.unwrap();
    store
        .append_message(NewMessage::user(&source.id, "hi"))
        .await
        .unwrap();
    let other = store.create_session(NewSession::new("别的")).await.unwrap();
    let foreign = store
        .append_message(NewMessage::user(&other.id, "别人的消息"))
        .await
        .unwrap();

    // 从别的会话借一个消息 id 当截断点：必须是明确错误，不能悄悄复制全部。
    let err = store
        .fork_session(&source.id, Some(&foreign.id), None)
        .await
        .expect_err("跨会话截断点应被拒绝");
    assert!(matches!(err, StoreError::MessageNotFound(id) if id == foreign.id));

    // 失败不能留下半截新会话。
    assert_eq!(store.count_sessions(None, None).await.unwrap(), 2);
}

fn ids(sessions: &[ai_ide_store::Session]) -> Vec<String> {
    sessions.iter().map(|session| session.id.clone()).collect()
}

/// 关键词检索：标题与消息正文都算命中，`count_sessions` 与分页结果必须同源。
#[tokio::test]
async fn keyword_search_spans_title_and_message_body() {
    let store = Store::open_in_memory().await.expect("打开内存库");
    let base = 1_700_000_000_000;

    let by_title = store
        .create_session(NewSession::new("重构 SessionStore").with_created_at(base))
        .await
        .unwrap();
    let by_body = store
        .create_session(NewSession::new("随手记").with_created_at(base + 1))
        .await
        .unwrap();
    let silent = store
        .create_session(NewSession::new("无关会话").with_created_at(base + 2))
        .await
        .unwrap();

    store
        .append_message(
            NewMessage::user(&by_body.id, "把 sessionStore 的落盘队列改成串行")
                .with_created_at(base + 10),
        )
        .await
        .unwrap();
    store
        .append_message(NewMessage::user(&silent.id, "今天天气如何").with_created_at(base + 11))
        .await
        .unwrap();

    // 大小写不敏感：正文里写的是 `sessionStore`，用小写去搜也要命中。
    let hits = store
        .list_sessions(SessionQuery::default().with_keyword("sessionstore"))
        .await
        .unwrap();
    let mut hit_ids = ids(&hits);
    hit_ids.sort();
    let mut expected = vec![by_title.id.clone(), by_body.id.clone()];
    expected.sort();
    assert_eq!(hit_ids, expected, "标题与正文命中都要返回");

    assert_eq!(
        store
            .count_sessions(None, Some("sessionstore"))
            .await
            .unwrap(),
        2,
        "计数与列表同条件"
    );

    // 只在正文里出现、标题里没有的词。
    let body_only = store
        .list_sessions(SessionQuery::default().with_keyword("串行"))
        .await
        .unwrap();
    assert_eq!(ids(&body_only), vec![by_body.id.clone()]);
    assert_eq!(store.count_sessions(None, Some("串行")).await.unwrap(), 1);

    // 过滤 + 工作区分页：关键词命中 2 条时，limit=1 只给 1 条，total 仍是 2。
    let paged = store
        .list_sessions(
            SessionQuery::default()
                .with_keyword("sessionstore")
                .with_limit(1)
                .with_offset(0),
        )
        .await
        .unwrap();
    assert_eq!(paged.len(), 1);
    assert_eq!(
        store
            .count_sessions(None, Some("sessionstore"))
            .await
            .unwrap(),
        2
    );

    // `%` / `_` 是字面量而非通配符：搜 `%` 命中 0 条。
    assert_eq!(store.count_sessions(None, Some("%")).await.unwrap(), 0);
    assert_eq!(store.count_sessions(None, Some("_")).await.unwrap(), 0);

    // 空关键词等于不过滤（调用方把空串归一为 None，这里验证 None 的语义）。
    assert_eq!(store.count_sessions(None, None).await.unwrap(), 3);
}
