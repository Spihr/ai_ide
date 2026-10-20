//! 端到端：真实工作区 + 脚本化模型，把六个核心工具串成一次完整的 M4 闭环。
//!
//! 单元测试盯的是单个工具的边界；这里盯的是「拼起来还对不对」——循环把工具结果
//! 折成消息、再喂回模型，这条链路只有真跑一遍才看得见。脚本化模型沿用
//! [`ModelTurn`] 抽象，所以完全不碰网络，也不依赖任何真实模型。

use ai_ide_agent::message;
use ai_ide_agent::registry::ToolContext;
use ai_ide_agent::runner::{AgentLoop, ModelTurn, RunLimits, StopReason, TurnReply, TurnRequest};
use ai_ide_agent::{AgentError, ToolRegistry};
use ai_ide_protocol::dto::ChatToolCall;
use ai_ide_protocol::ErrorCode;
use ai_ide_workspace::{Workspace, WorkspaceManager};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 临时工作区（集成测试够不着 crate 内部的 `testing` 模块，这里自带一份精简版）。
struct Sandbox {
    dir: PathBuf,
    workspace: Arc<Workspace>,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-agent-e2e-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let workspace = WorkspaceManager::new()
            .open(&dir.display().to_string())
            .expect("打开临时工作区");
        Self { dir, workspace }
    }

    fn ctx(&self) -> ToolContext<'_> {
        ToolContext::new(&self.workspace)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::write(path, content).expect("写测试文件");
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).expect("读测试文件")
    }

    fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// 脚本化模型：按顺序吐出预设回复，并记下每轮收到的请求。
struct Scripted {
    turns: Mutex<VecDeque<Result<TurnReply, AgentError>>>,
    seen: Mutex<Vec<TurnRequest>>,
}

impl Scripted {
    fn new(turns: Vec<Result<TurnReply, AgentError>>) -> Self {
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
                .unwrap_or_else(|| Ok(TurnReply::text_only("（脚本已耗尽）")))
        };
        reply
    }
}

fn call(id: &str, name: &str, args: Value) -> ChatToolCall {
    ChatToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

fn answered_ids(messages: &[ai_ide_protocol::dto::ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|item| item.role == ai_ide_protocol::dto::ChatRole::Tool)
        .filter_map(|item| item.tool_call_id.clone())
        .collect()
}

#[tokio::test]
async fn six_tools_form_a_complete_loop() {
    let sandbox = Sandbox::new("six-tools");
    sandbox.write("readme.md", "# 工作区\n");

    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "先看看有什么",
            vec![call("c1", "list", json!({"path": "."}))],
        )),
        Ok(TurnReply::with_tool_calls(
            "建个目录",
            vec![call("c2", "create", json!({"path": "src", "kind": "dir"}))],
        )),
        Ok(TurnReply::with_tool_calls(
            "写个文件",
            vec![call(
                "c3",
                "write",
                json!({"path": "src/app.ts", "content": "// TODO: 换个名字\nconst value = 1;\n"}),
            )],
        )),
        Ok(TurnReply::with_tool_calls(
            "搜一下 TODO",
            vec![call("c4", "grep", json!({"query": "TODO"}))],
        )),
        Ok(TurnReply::with_tool_calls(
            "读回来确认",
            vec![call("c5", "read", json!({"path": "src/app.ts"}))],
        )),
        Ok(TurnReply::with_tool_calls(
            "改掉 TODO",
            vec![call(
                "c6",
                "edit",
                json!({"path": "src/app.ts", "old_text": "// TODO: 换个名字", "new_text": "// 已经改好"}),
            )],
        )),
        Ok(TurnReply::text_only("收工")),
    ]);
    let registry = ToolRegistry::core();

    let outcome = AgentLoop::new(&model, &registry)
        .run(
            &sandbox.ctx(),
            vec![message::user_message("在工作区里建个文件")],
        )
        .await
        .expect("闭环应成功");

    assert_eq!(outcome.stop_reason, StopReason::Completed);
    assert_eq!(outcome.rounds, 7);
    assert_eq!(outcome.steps, 6, "六个工具各跑一次");
    assert!(
        outcome.assistant_errors.is_empty(),
        "不该有回灌的失败：{:?}",
        outcome.assistant_errors
    );
    assert_eq!(
        sandbox.read("src/app.ts"),
        "// 已经改好\nconst value = 1;\n"
    );
    assert_eq!(
        answered_ids(&outcome.messages),
        vec!["c1", "c2", "c3", "c4", "c5", "c6"],
        "每个调用都要有对应结果，且顺序一致"
    );

    // 检索与读取的输出确实走到了模型面前（而不是被循环吞掉）。
    let contents: Vec<String> = outcome.tool_messages().map(|m| m.content.clone()).collect();
    assert!(contents[3].contains("TODO"), "grep 的命中应出现在结果里");
    assert!(
        contents[4].contains("const value = 1;"),
        "read 的内容应出现在结果里"
    );
}

#[tokio::test]
async fn ambiguous_edit_is_fed_back_then_retried_with_a_longer_anchor() {
    let sandbox = Sandbox::new("edit-retry");
    let original = "fn a() {\n  let x = 1;\n}\n\nfn b() {\n  let x = 1;\n}\n";
    sandbox.write("src/dup.ts", original);

    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "edit",
                json!({"path": "src/dup.ts", "old_text": "let x = 1;", "new_text": "let x = 2;"}),
            )],
        )),
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c2",
                "edit",
                json!({"path": "src/dup.ts", "old_text": "fn b() {\n  let x = 1;\n}", "new_text": "fn b() {\n  let x = 2;\n}"}),
            )],
        )),
        Ok(TurnReply::text_only("这次改对了")),
    ]);
    let registry = ToolRegistry::core();

    let outcome = AgentLoop::new(&model, &registry)
        .run(
            &sandbox.ctx(),
            vec![message::user_message("把 b 里的 x 改成 2")],
        )
        .await
        .unwrap();

    assert_eq!(outcome.stop_reason, StopReason::Completed);
    assert_eq!(outcome.steps, 2);
    assert_eq!(
        outcome.assistant_errors.len(),
        1,
        "第一次非唯一匹配应被回灌"
    );

    let first = outcome.tool_messages().next().expect("应有第一次的结果");
    assert!(first.content.contains("<tool_error"));
    assert!(first.content.contains("出现了 2 次"));

    let text = sandbox.read("src/dup.ts");
    assert!(
        text.contains("fn b() {\n  let x = 2;\n}"),
        "第二次应改在正确的位置"
    );
    assert!(text.contains("fn a() {\n  let x = 1;\n}"), "a 不该被动到");
}

#[tokio::test]
async fn escaping_the_workspace_aborts_the_loop_with_1002() {
    let sandbox = Sandbox::new("escape");
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "read",
                json!({"path": "../../../Windows/win.ini"}),
            )],
        )),
        Ok(TurnReply::text_only("不该跑到这里")),
    ]);
    let registry = ToolRegistry::core();

    let err = AgentLoop::new(&model, &registry)
        .run(
            &sandbox.ctx(),
            vec![message::user_message("读工作区外面的文件")],
        )
        .await
        .expect_err("越界必须终止循环");

    assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    assert_eq!(err.code().as_i32(), 1002);
    assert_eq!(model.calls(), 1, "致命错误后不该再喊模型");
}

#[tokio::test]
async fn sensitive_write_is_refused_and_nothing_lands_on_disk() {
    let sandbox = Sandbox::new("sensitive");
    let model = Scripted::new(vec![Ok(TurnReply::with_tool_calls(
        "",
        vec![call(
            "c1",
            "write",
            json!({"path": ".env", "content": "SECRET=1\n"}),
        )],
    ))]);
    let registry = ToolRegistry::core();

    let err = AgentLoop::new(&model, &registry)
        .run(&sandbox.ctx(), vec![message::user_message("写个环境文件")])
        .await
        .expect_err("敏感路径必须被拒");

    assert_eq!(err.code(), ErrorCode::PATH_DENIED);
    assert!(!sandbox.exists(".env"), "被拒的写入不能留下任何东西");
}

#[tokio::test]
async fn round_limit_is_enforced_end_to_end() {
    let sandbox = Sandbox::new("rounds");
    sandbox.write("a.txt", "hello\n");

    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call("c1", "read", json!({"path": "a.txt"}))],
        )),
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call("c2", "read", json!({"path": "a.txt"}))],
        )),
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call("c3", "read", json!({"path": "a.txt"}))],
        )),
    ]);
    let registry = ToolRegistry::core();

    let outcome = AgentLoop::new(&model, &registry)
        .with_limits(RunLimits::new(2, 10))
        .run(&sandbox.ctx(), vec![message::user_message("一直读")])
        .await
        .unwrap();

    assert_eq!(outcome.stop_reason, StopReason::MaxRounds { rounds: 2 });
    assert_eq!(model.calls(), 2, "撞到上限后不能再喊模型");
    assert_eq!(answered_ids(&outcome.messages), vec!["c1", "c2"]);
}
