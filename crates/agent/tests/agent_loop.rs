//! 端到端：真实工作区 + 脚本化模型，把六个核心工具串成一次完整的 M4 闭环。
//!
//! 单元测试盯的是单个工具的边界；这里盯的是「拼起来还对不对」——循环把工具结果
//! 折成消息、再喂回模型，这条链路只有真跑一遍才看得见。脚本化模型沿用
//! [`ModelTurn`] 抽象，所以完全不碰网络，也不依赖任何真实模型。

use ai_ide_agent::message;
use ai_ide_agent::registry::ToolContext;
use ai_ide_agent::runner::{AgentLoop, ModelTurn, RunLimits, StopReason, TurnReply, TurnRequest};
use ai_ide_agent::{AgentError, ToolRegistry};
use ai_ide_protocol::dto::{ChatMessage, ChatToolCall};
use ai_ide_protocol::ErrorCode;
use ai_ide_workspace::{Workspace, WorkspaceManager};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

// --------------------------------------------------------------------------
// M4 切片二：`run_command` 与 diff 引擎的端到端验证
// --------------------------------------------------------------------------

/// 唤起本测试二进制自身跑一个靶子，与 `run_command` 单元测试同一手法：
/// 不赌宿主上装着 node / python，也就不会因为环境差异静默漏测。
fn self_command(target: &str) -> (String, Vec<String>) {
    let exe = std::env::current_exe()
        .expect("测试可执行文件")
        .display()
        .to_string();
    (
        exe,
        vec![
            "--ignored".to_string(),
            "--nocapture".to_string(),
            target.to_string(),
        ],
    )
}

/// 工具结果消息的 JSON 正文。
fn json_of(message: &ChatMessage) -> Value {
    serde_json::from_str(&message.content)
        .unwrap_or_else(|err| panic!("工具结果应是 JSON（{err}）：{}", message.content))
}

/// 20 行基线文件，diff 的两块改动都发生在它身上。
fn twenty_lines() -> String {
    (1..=20).map(|n| format!("line {n}\n")).collect()
}

/// 手工写死的两块补丁：第 2 行与第 18 行各一处，中间隔着 15 行
/// （> 2×context = 6），所以必然切成两块——这才测得到「逐块选择」。
const TWO_HUNK_PATCH: &str = "--- a/many.txt\n+++ b/many.txt\n@@ -1,5 +1,5 @@\n line 1\n-line 2\n+line 2 changed\n line 3\n line 4\n line 5\n@@ -15,6 +15,6 @@\n line 15\n line 16\n line 17\n-line 18\n+line 18 changed\n line 19\n line 20\n";

#[test]
#[ignore = "由端到端用例按需唤起"]
fn e2e_target_ok() {
    println!("E2E-OUT");
    eprintln!("E2E-ERR");
}

#[test]
#[ignore = "由端到端用例按需唤起"]
fn e2e_target_floods() {
    use std::io::Write;
    let chunk = "a".repeat(1024 * 1024);
    let mut out = std::io::stdout().lock();
    for _ in 0..2 {
        out.write_all(chunk.as_bytes()).expect("写 stdout");
    }
    out.flush().expect("刷 stdout");
}

#[test]
#[ignore = "由端到端用例按需唤起"]
#[allow(clippy::zombie_processes)]
fn e2e_target_tree() {
    use std::io::Write;
    let exe = std::env::current_exe().expect("测试可执行文件");
    let child = std::process::Command::new(exe)
        .args(["--ignored", "--nocapture", "e2e_target_idle"])
        .spawn()
        .expect("派生孙进程");
    println!("GRANDCHILD-PID={}", child.id());
    std::io::stdout().flush().expect("刷 stdout");
    std::thread::sleep(Duration::from_secs(30));
}

#[test]
#[ignore = "由端到端用例按需唤起"]
fn e2e_target_idle() {
    std::thread::sleep(Duration::from_secs(60));
}

/// 等进程消失。返回 `true` 表示耐心用完它还活着。
#[cfg(windows)]
fn wait_until_dead(pid: u32, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    loop {
        if !process_alive(pid) {
            return false;
        }
        if Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut command = std::process::Command::new("tasklist");
    command
        .arg("/FI")
        .arg(format!("PID eq {pid}"))
        .arg("/FO")
        .arg("CSV")
        .arg("/NH")
        .stdin(std::process::Stdio::null());
    command.creation_flags(CREATE_NO_WINDOW);
    let Ok(output) = command.output() else {
        return false;
    };

    let wanted = pid.to_string();
    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.split(',')
            .nth(1)
            .map(|field| field.trim().trim_matches('"') == wanted)
            .unwrap_or(false)
    })
}

#[cfg(windows)]
#[tokio::test]
async fn run_command_timeout_kills_the_whole_process_tree_end_to_end() {
    let sandbox = Sandbox::new("e2e-tree");
    let (exe, argv) = self_command("e2e_target_tree");
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "run_command",
                json!({ "cmd": exe, "args": argv, "timeout_ms": 4000 }),
            )],
        )),
        Ok(TurnReply::text_only("超时就该连孙进程一起收干净")),
    ]);
    let registry = ToolRegistry::core();

    let started = Instant::now();
    let outcome = AgentLoop::new(&model, &registry)
        .run(
            &sandbox.ctx(),
            vec![message::user_message("跑一条会挂住的命令")],
        )
        .await
        .expect("超时是命令的事实，不是循环故障");
    let elapsed = started.elapsed();

    assert_eq!(outcome.stop_reason, StopReason::Completed);
    assert_eq!(outcome.steps, 1);
    assert!(
        outcome.assistant_errors.is_empty(),
        "超时结果不该被当成工具错误：{:?}",
        outcome.assistant_errors
    );

    let value = json_of(outcome.tool_messages().next().expect("应有一条工具结果"));
    assert_eq!(value["timed_out"], true, "睡 30 秒的命令必须被判超时");
    assert_eq!(value["killed_tree"], true, "超时必须真的发出杀树动作");
    assert_eq!(value["ok"], false);
    assert_ne!(value["exit_code"], 0, "被杀的进程不能报成正常退出");
    assert_eq!(value["streams_drained"], true, "管道必须在宽限期内关干净");
    assert!(
        elapsed >= Duration::from_millis(4000),
        "不该提前放弃，实际 {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "杀完就该返回，不该陪着挂到靶子自己睡醒，实际 {elapsed:?}"
    );

    let grandchild: u32 = value["stdout"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix("GRANDCHILD-PID="))
        .expect("靶子应打印孙进程 pid")
        .trim()
        .parse()
        .expect("孙进程 pid 应是数字");
    assert!(
        !wait_until_dead(grandchild, Duration::from_secs(5)),
        "孙进程（pid {grandchild}）必须一起被收掉，不能留孤儿"
    );
}

#[tokio::test]
async fn run_command_splits_and_caps_the_streams_end_to_end() {
    let sandbox = Sandbox::new("e2e-streams");
    let (exe, ok_args) = self_command("e2e_target_ok");
    let (flood_exe, flood_args) = self_command("e2e_target_floods");
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "run_command",
                json!({ "cmd": exe, "args": ok_args }),
            )],
        )),
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c2",
                "run_command",
                json!({ "cmd": flood_exe, "args": flood_args }),
            )],
        )),
        Ok(TurnReply::text_only("分流与截断都看到了")),
    ]);
    let registry = ToolRegistry::core();

    let outcome = AgentLoop::new(&model, &registry)
        .run(&sandbox.ctx(), vec![message::user_message("跑两条命令")])
        .await
        .expect("循环应成功");

    assert_eq!(outcome.stop_reason, StopReason::Completed);
    assert_eq!(outcome.steps, 2);
    assert!(
        outcome.assistant_errors.is_empty(),
        "两条命令都该跑成：{:?}",
        outcome.assistant_errors
    );

    let results: Vec<Value> = outcome.tool_messages().map(json_of).collect();
    assert_eq!(results.len(), 2);

    let split = &results[0];
    let stdout = split["stdout"].as_str().unwrap_or_default();
    let stderr = split["stderr"].as_str().unwrap_or_default();
    assert_eq!(split["exit_code"], 0);
    assert_eq!(split["ok"], true);
    assert!(
        stdout.contains("E2E-OUT"),
        "stdout 该收到标准输出：{stdout:?}"
    );
    assert!(
        !stdout.contains("E2E-ERR"),
        "stderr 不能混进 stdout：{stdout:?}"
    );
    assert!(
        stderr.contains("E2E-ERR"),
        "stderr 该收到标准错误：{stderr:?}"
    );
    assert!(
        !stderr.contains("E2E-OUT"),
        "stdout 不能混进 stderr：{stderr:?}"
    );
    assert_eq!(split["stdout_truncated"], false);
    assert_eq!(split["stderr_truncated"], false);

    let flooded = &results[1];
    assert_eq!(flooded["exit_code"], 0, "灌数据不该把命令搞挂");
    assert_eq!(
        flooded["stdout_truncated"], true,
        "2 MiB 远超上限，必须标截断"
    );
    let total = flooded["stdout_bytes"].as_u64().unwrap_or_default();
    assert!(total >= 2 * 1024 * 1024, "真实总量要如实上报，实际 {total}");
    let kept = flooded["stdout"].as_str().unwrap_or_default().len();
    assert!(kept <= 64 * 1024, "回传正文不能超过上限，实际 {kept}");
}

#[tokio::test]
async fn apply_diff_hunk_selection_decides_what_lands_on_disk() {
    let sandbox = Sandbox::new("e2e-diff-subset");
    let original = twenty_lines();
    sandbox.write("many.txt", &original);
    let updated = original
        .replace("line 2\n", "line 2 changed\n")
        .replace("line 18\n", "line 18 changed\n");
    let registry = ToolRegistry::core();

    // 第一轮：生成模式，只看差异，一个字节都不许落盘。
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "apply_diff",
                json!({ "path": "many.txt", "content": updated.clone() }),
            )],
        )),
        Ok(TurnReply::text_only("先看看差异")),
    ]);
    let outcome = AgentLoop::new(&model, &registry)
        .run(
            &sandbox.ctx(),
            vec![message::user_message("算一份补丁出来")],
        )
        .await
        .expect("生成补丁不该失败");
    let generated = json_of(outcome.tool_messages().next().expect("应有一条工具结果"));
    assert_eq!(generated["mode"], "generate");
    assert_eq!(generated["existed"], true);
    assert_eq!(generated["changed"], true);
    assert_eq!(generated["hunks"], 2, "两处改动隔着 15 行，必须是两块");
    assert_eq!(
        generated["patch"].as_str().unwrap_or_default(),
        TWO_HUNK_PATCH,
        "引擎渲染出的补丁应与手工基线逐字节一致"
    );
    assert_eq!(
        sandbox.read("many.txt"),
        original,
        "生成模式一个字节都不许落盘"
    );

    // 第二轮：只接受第二块，第一块必须原样留着。
    sandbox.write("many.txt", &original);
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "apply_diff",
                json!({ "path": "many.txt", "patch": TWO_HUNK_PATCH, "accept": [1] }),
            )],
        )),
        Ok(TurnReply::text_only("只收第二块")),
    ]);
    let outcome = AgentLoop::new(&model, &registry)
        .run(&sandbox.ctx(), vec![message::user_message("只接受第二块")])
        .await
        .expect("逐块应用不该失败");
    let applied = json_of(outcome.tool_messages().next().expect("应有一条工具结果"));
    assert_eq!(applied["mode"], "apply");
    assert_eq!(applied["changed"], true);
    assert_eq!(applied["hunks"], 2);
    assert_eq!(applied["accepted"], json!([1]));
    assert_eq!(applied["rejected"], json!([0]));
    assert!(applied["hash"].is_string(), "真的写了盘才回哈希");
    let after_second_only = original.replace("line 18\n", "line 18 changed\n");
    assert_eq!(sandbox.read("many.txt"), after_second_only);
    assert!(
        !sandbox.read("many.txt").contains("line 2 changed"),
        "没被选中的第一块一个字都不许落盘"
    );

    // 第三轮：一块都不接受，文件保持原样。
    sandbox.write("many.txt", &original);
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "apply_diff",
                json!({ "path": "many.txt", "patch": TWO_HUNK_PATCH, "accept": [] }),
            )],
        )),
        Ok(TurnReply::text_only("全拒")),
    ]);
    let outcome = AgentLoop::new(&model, &registry)
        .run(&sandbox.ctx(), vec![message::user_message("全都不要")])
        .await
        .expect("全拒不该失败");
    let rejected = json_of(outcome.tool_messages().next().expect("应有一条工具结果"));
    assert_eq!(rejected["changed"], false);
    assert_eq!(rejected["accepted"], json!([]));
    assert_eq!(rejected["rejected"], json!([0, 1]));
    assert!(rejected["hash"].is_null(), "没写盘就不该有哈希");
    assert_eq!(sandbox.read("many.txt"), original);

    // 第四轮：不传 accept 就是全收，两块都落盘。
    sandbox.write("many.txt", &original);
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "apply_diff",
                json!({ "path": "many.txt", "patch": TWO_HUNK_PATCH }),
            )],
        )),
        Ok(TurnReply::text_only("全收")),
    ]);
    let outcome = AgentLoop::new(&model, &registry)
        .run(&sandbox.ctx(), vec![message::user_message("全都接受")])
        .await
        .expect("全收不该失败");
    let accepted = json_of(outcome.tool_messages().next().expect("应有一条工具结果"));
    assert_eq!(accepted["accepted"], json!([0, 1]));
    assert_eq!(accepted["rejected"], json!([]));
    assert_eq!(sandbox.read("many.txt"), updated);
}

#[tokio::test]
async fn malicious_commands_are_denied_and_the_loop_stops() {
    let sandbox = Sandbox::new("e2e-malicious");
    let registry = ToolRegistry::core();

    for (args, marker) in [
        (json!({ "cmd": "format", "args": ["C:"] }), "禁止执行的程序"),
        (
            json!({ "cmd": "diskpart", "args": ["/s", "script.txt"] }),
            "禁止执行的程序",
        ),
        (
            json!({ "cmd": "shutdown", "args": ["/r", "/t", "0"] }),
            "禁止执行的程序",
        ),
        (
            json!({ "cmd": "reg", "args": ["delete", "HKLM\\Software\\X", "/f"] }),
            "reg delete",
        ),
        (
            json!({ "cmd": "net", "args": ["user", "hacker", "p@ss", "/add"] }),
            "net user",
        ),
        (json!({ "cmd": "rm", "args": ["-rf", "/"] }), "rm -rf"),
    ] {
        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call("c1", "run_command", args.clone())],
            )),
            Ok(TurnReply::text_only("不该走到这里")),
        ]);

        let err = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("跑个危险命令")])
            .await
            .expect_err("恶意命令必须终止循环，而不是回灌给模型重试");

        assert_eq!(err.code(), ErrorCode::TOOL_NOT_ALLOWED, "{args} 应被拒");
        assert_eq!(err.code().as_i32(), 3001);
        assert!(err.is_fatal(), "{args} 被拒是致命的");
        assert!(
            err.to_string().contains(marker),
            "{args} 的拒绝理由该点明 `{marker}`，实际：{err}"
        );
        assert_eq!(model.calls(), 1, "致命错误后不该再喊模型");
    }
}

#[tokio::test]
async fn escaping_paths_and_sensitive_targets_are_refused_end_to_end() {
    let sandbox = Sandbox::new("e2e-guard");
    let registry = ToolRegistry::core();

    // 工作目录越界：命令根本没机会起来。
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "run_command",
                json!({ "cmd": "x", "cwd": "../../Windows/System32" }),
            )],
        )),
        Ok(TurnReply::text_only("不该走到这里")),
    ]);
    let err = AgentLoop::new(&model, &registry)
        .run(
            &sandbox.ctx(),
            vec![message::user_message("在工作区外面跑命令")],
        )
        .await
        .expect_err("cwd 越界必须终止循环");
    assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    assert_eq!(err.code().as_i32(), 1002);
    assert!(err.is_fatal());
    assert_eq!(model.calls(), 1);

    // 写盘越界：目标文件不能留下一丝痕迹。
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "apply_diff",
                json!({ "path": "../outside.txt", "content": "nope\n" }),
            )],
        )),
        Ok(TurnReply::text_only("不该走到这里")),
    ]);
    let err = AgentLoop::new(&model, &registry)
        .run(
            &sandbox.ctx(),
            vec![message::user_message("改工作区外面的文件")],
        )
        .await
        .expect_err("越界写入必须被拒");
    assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    assert_eq!(err.code().as_i32(), 1002);
    assert!(err.is_fatal());
    assert!(!sandbox.exists("../outside.txt"), "越界目标不该被建出来");
    assert_eq!(model.calls(), 1);

    // 敏感目标：密钥文件连补丁都不许贴。
    let model = Scripted::new(vec![
        Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "apply_diff",
                json!({
                    "path": ".env",
                    "patch": "--- a/.env\n+++ b/.env\n@@ -1,1 +1,1 @@\n-A=1\n+A=2\n"
                }),
            )],
        )),
        Ok(TurnReply::text_only("不该走到这里")),
    ]);
    let err = AgentLoop::new(&model, &registry)
        .run(&sandbox.ctx(), vec![message::user_message("改密钥文件")])
        .await
        .expect_err("敏感路径必须被拒");
    assert_eq!(err.code(), ErrorCode::PATH_DENIED);
    assert_eq!(err.code().as_i32(), 1003);
    assert!(err.is_fatal());
    assert!(!sandbox.exists(".env"));
    assert_eq!(model.calls(), 1);
}
