//! `run_command`：在工作区里跑一条外部命令——独立进程组、超时杀整棵进程树、分流截断。
//!
//! ## 契约（对应 `docs/02` §8.3、`docs/04` §4.2、`docs/05` 的 M4 切片二）
//!
//! - **不走 shell**：`cmd` 是可执行程序、`args` 是参数数组，两者分开传。
//!   参数里的空格与引号因此不需要转义，黑名单也能看清真实意图；反过来，
//!   想手工拼 shell 命令行的调用（`cmd /c ...`、参数里带 `|` / `>` / `&&`）一律拒绝。
//! - **工作目录**：`cwd` 相对工作区根，缺省即工作区根；先过 `PathGuard`——
//!   越界 `1002`、敏感目录 `1003`，且都在起进程**之前**判定。
//! - **进程树**：Windows 上子进程会被收进 Job Object，句柄一关整棵子树必死；
//!   超时再补一刀 `taskkill /T /F`。命令就算自己先退出、或者派了孙进程，
//!   也不会留下孤儿。Unix 侧退化为只 `kill` 直接子进程（见本文件末尾的偏离说明）。
//! - **超时**：`timeout_ms` 缺省 120 秒、夹在 1ms～10min；超时杀树后**如实**回
//!   `timed_out: true` 与「被杀掉」的退出码，不伪装成正常结束。
//! - **输出**：stdout / stderr 分流，各自最多回传 64 KiB，同时报出真实总量
//!   （`*_bytes`）与截断标记——模型据此知道「后面还有，只是被截了」。
//! - **退出码**：非零退出**不是**工具错误——命令确实失败了，这是事实，
//!   折进结果 JSON 回灌给模型；只有「起不来 / 参数不合法 / 命中黑名单」才是错误。
//!
//! ## 偏离说明（诚实披露）
//!
//! 1. **Unix 不杀整棵树**：本切片的目标平台是 Windows，Unix 侧只在超时后 `kill`
//!    直接子进程，没有引入 `libc` 的 `killpg`（避免为一个非目标平台加依赖）。
//! 2. **子进程归属**：本机测试进程若已在某个 Job 里，`AssignProcessToJobObject`
//!    可能失败；此时自动退回 `taskkill /T /F`，行为仍然是「杀树」，只是少了一层硬保障。

use super::Args;
use crate::error::AgentError;
use crate::permission::{dangerous_pattern, dangerous_program, exe_stem};
use crate::registry::{Tool, ToolContext, ToolOutcome};
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const NAME: &str = "run_command";

/// 默认超时（`docs/02` §8.3）。
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// 超时上限：再大也只等 10 分钟。
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// 单条流（stdout / stderr）的回传上限，与 `docs/04` §3.2 的 `output_cap_bytes` 同口径。
pub const MAX_STREAM_BYTES: usize = 64 * 1024;

/// 进程退出后，等读取线程收尾的宽限期（孙进程可能还攥着管道）。
const DRAIN_GRACE: Duration = Duration::from_millis(1_000);

/// 轮询子进程状态的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(20);

// 危险程序 / 危险命令行特征的黑名单住在 `crate::permission`：权限策略与这里
// 的起进程前闸门必须用同一个事实来源，否则两处各存一份、改一处漏一处。

/// 手工拼 shell 命令行的特征。
const SHELL_PATTERNS: &[&str] = &["|", "&&", "||", ">", "<", "\n"];

/// 拿来做 `cmd /c ...` 这类包装的程序名（比对去掉扩展名后的名字）。
const SHELL_PROGRAMS: &[&str] = &["cmd", "powershell", "pwsh", "sh", "bash", "zsh"];

/// 执行一条外部命令。
pub struct RunCommandTool;

impl Tool for RunCommandTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "在工作区里执行一条外部命令（不经 shell），返回退出码与分流的 stdout / stderr。单条流最多 64 KiB，超时（默认 120 秒）会杀掉整棵进程树；非零退出会如实回传，不是工具错误。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "cmd": {
                    "type": "string",
                    "description": "可执行程序（名字或相对 cwd 的路径），不经 shell 解释"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "参数数组，逐项传递，不做 shell 展开"
                },
                "cwd": {
                    "type": "string",
                    "description": "工作目录，相对工作区根；缺省即工作区根"
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600000,
                    "description": "超时毫秒数，缺省 120000，上限 600000"
                }
            },
            "required": ["cmd"],
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let program = args.str("cmd")?;
        let argv = args.opt_str_list("args")?;
        let cwd_arg = args.opt_str("cwd")?;
        let timeout_ms = args
            .opt_u32("timeout_ms")?
            .map(u64::from)
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, MAX_TIMEOUT_MS);
        let timeout = Duration::from_millis(timeout_ms);

        // 安全闸门一：危险命令在**起进程之前**拒绝，这时副作用还没发生。
        let rendered = render_command(program, &argv);
        screen(program, &rendered)?;
        // 安全闸门二：手工拼 shell 的用法直接拒绝，并告诉模型正确姿势。
        shell_inline_hint(program, &argv)?;

        // 路径闸门：cwd 越界 1002、敏感目录 1003，同样在起进程之前。
        let cwd = match cwd_arg {
            Some(text) => ctx.guard.resolve_for_write(text)?,
            None => ctx.root.to_path_buf(),
        };
        let relative = ctx.display_path(&cwd);
        if !cwd.is_dir() {
            return Err(AgentError::ToolFailed {
                tool: NAME.to_string(),
                message: format!("工作目录不存在：{relative}"),
            });
        }

        // stdout / stderr 各起一个读取线程，**必须**持续读到 EOF：
        // 只回传前 64 KiB 但不停手，否则命令写满管道缓冲区就会卡死。
        let stdout = Arc::new(Mutex::new(Capture::new()));
        let stderr = Arc::new(Mutex::new(Capture::new()));

        let mut command = Command::new(program);
        command
            .args(&argv)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        detach(&mut command);

        let started = Instant::now();
        let mut child = command.spawn().map_err(|err| AgentError::ToolFailed {
            tool: NAME.to_string(),
            message: format!("无法启动 `{rendered}`：{err}"),
        })?;

        let pid = child.id();
        // Windows：把子进程收进 Job Object，句柄一关整棵子树必死（「不留孤儿」的硬保障）。
        #[cfg(windows)]
        let job: JobGuard = job::Job::adopt(pid);
        #[cfg(not(windows))]
        let job: JobGuard = ();

        let readers = vec![
            spawn_reader(child.stdout.take(), Arc::clone(&stdout)),
            spawn_reader(child.stderr.take(), Arc::clone(&stderr)),
        ];

        let verdict = supervise(&mut child, pid, timeout, job)?;
        let drained = join_readers(readers, DRAIN_GRACE);
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let stdout = snapshot(&stdout);
        let stderr = snapshot(&stderr);
        let (exit_code, signal) = match &verdict.status {
            Some(status) => exit_facts(status),
            None => (None, None),
        };

        let payload = json!({
            // 非零退出是「命令失败了」，不是「工具挂了」——所以这里是 ok: false 而不是错误。
            "ok": !verdict.timed_out && exit_code == Some(0),
            "timed_out": verdict.timed_out,
            "exit_code": exit_code,
            "signal": signal,
            "killed_tree": verdict.killed_tree,
            "elapsed_ms": elapsed_ms,
            "cwd": relative,
            "command": rendered,
            "stdout": stdout.0,
            "stdout_bytes": stdout.1,
            "stdout_truncated": stdout.2,
            "stderr": stderr.0,
            "stderr_bytes": stderr.1,
            "stderr_truncated": stderr.2,
            "streams_drained": drained,
        });
        Ok(ToolOutcome::new(payload.to_string()))
    }
}

// ---------------------------------------------------------------- 安全闸门

/// 把程序名与参数拼成一行，用于黑名单匹配与结果回显。
fn render_command(program: &str, argv: &[String]) -> String {
    if argv.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", argv.join(" "))
    }
}

/// 危险命令黑名单：命中即终止循环（`3001`），不降级成「回灌重试」。
///
/// 名单本身在 [`crate::permission`]，与权限闸门的「高危命令直接拒」共用一份。
fn screen(program: &str, rendered: &str) -> Result<(), AgentError> {
    if let Some(blocked) = dangerous_program(program) {
        return Err(denied(format!("`{blocked}` 属于禁止执行的程序")));
    }

    if let Some(pattern) = dangerous_pattern(rendered) {
        return Err(denied(format!("命令行特征 `{pattern}` 属于禁止执行的操作")));
    }
    Ok(())
}

/// 手工拼 shell 命令行：语法上没坏，所以是可回灌的用法错误，附带正确姿势。
fn shell_inline_hint(program: &str, argv: &[String]) -> Result<(), AgentError> {
    let name = exe_stem(program);
    if SHELL_PROGRAMS.contains(&name.as_str()) {
        return Err(AgentError::BadArgs {
            tool: NAME.to_string(),
            reason: format!("不要用 shell 包装命令：直接传 `cmd` + `args`（这里是 `{program}`）"),
        });
    }
    if let Some(pattern) = argv
        .iter()
        .find(|arg| SHELL_PATTERNS.iter().any(|pattern| arg.contains(*pattern)))
    {
        return Err(AgentError::BadArgs {
            tool: NAME.to_string(),
            reason: format!(
                "参数 `{pattern}` 里有 shell 语法（管道 / 重定向 / 换行），本工具不经 shell：改用单条命令，或多跑几次"
            ),
        });
    }
    Ok(())
}

/// 可执行程序名的取法（去扩展名、转小写）在 [`crate::permission::exe_stem`]：
/// 黑名单比对与权限闸门必须用同一套规则，别再在这里立一份。
fn denied(reason: String) -> AgentError {
    AgentError::CommandDenied {
        tool: NAME.to_string(),
        reason,
    }
}

// ---------------------------------------------------------------- 看守与杀树

/// 看守子进程：到点没退就杀树，并如实报出「超时」这件事。
fn supervise(
    child: &mut Child,
    pid: u32,
    timeout: Duration,
    job: JobGuard,
) -> Result<Verdict, AgentError> {
    let deadline = Instant::now() + timeout;
    loop {
        // 先看退没退出：正好卡在 deadline 前自然结束的命令，不该被算成超时。
        let status = child.try_wait().map_err(wait_failed)?;
        if let Some(status) = status {
            return Ok(Verdict {
                status: Some(status),
                timed_out: false,
                killed_tree: false,
            });
        }
        if Instant::now() >= deadline {
            let killed_tree = kill_tree(child, pid, job);
            // 杀完必须 wait：既回收僵尸，也拿到「被杀掉」这个事实。
            let status = child.wait().ok();
            return Ok(Verdict {
                status,
                timed_out: true,
                killed_tree,
            });
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn wait_failed(err: std::io::Error) -> AgentError {
    AgentError::ToolFailed {
        tool: NAME.to_string(),
        message: format!("无法等待进程结束：{err}"),
    }
}

struct Verdict {
    status: Option<ExitStatus>,
    timed_out: bool,
    killed_tree: bool,
}

/// 退出码与信号：被信号 / 杀树干掉时可能压根没有退出码，
/// 补一个非零占位，绝不让 `ok` 或调用方误以为命令成功。
fn exit_facts(status: &ExitStatus) -> (Option<i32>, Option<u32>) {
    let signal = signal_of(status);
    let code = status.code();
    let code = match (code, signal) {
        (Some(code), _) => Some(code),
        (None, Some(_)) => Some(1),
        (None, None) => None,
    };
    (code, signal)
}

#[cfg(windows)]
fn signal_of(_status: &ExitStatus) -> Option<u32> {
    None
}

#[cfg(not(windows))]
fn signal_of(status: &ExitStatus) -> Option<u32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| signal as u32)
}

// ---------------------------------------------------------------- 平台差异

/// Windows：让子进程自成进程组、不开控制台窗口；Unix：自成进程组。
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// 辅助进程（`taskkill` / `tasklist`）不要弹窗。
#[cfg(windows)]
fn quiet(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(windows)]
type JobGuard = Option<job::Job>;

#[cfg(not(windows))]
type JobGuard = ();

/// 杀掉整棵进程树，返回是否确实发出了杀的动作。
#[cfg(windows)]
fn kill_tree(child: &mut Child, pid: u32, job: JobGuard) -> bool {
    let mut killed = false;
    if let Some(job) = job.as_ref() {
        killed |= job.terminate();
    }
    killed |= taskkill_tree(pid);
    if !killed {
        killed = child.kill().is_ok();
    }
    // job 在这里析构 → 关句柄 → KILL_ON_JOB_CLOSE 再兜一层底。
    killed
}

/// Unix：没有 Job Object，只能杀直接子进程（本切片的目标平台是 Windows）。
#[cfg(not(windows))]
fn kill_tree(child: &mut Child, _pid: u32, _job: JobGuard) -> bool {
    child.kill().is_ok()
}

/// `taskkill /PID <pid> /T /F`：按父子关系走一遍整棵树。
#[cfg(windows)]
fn taskkill_tree(pid: u32) -> bool {
    let mut command = Command::new("taskkill");
    command
        .arg("/PID")
        .arg(pid.to_string())
        .arg("/T")
        .arg("/F")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    quiet(&mut command);
    matches!(command.status(), Ok(status) if status.success())
}

/// Job Object 封装：`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 保证句柄一关，
/// 里面的进程（含它们后来派生的子孙）全部终止。
///
/// 本模块是全仓唯一使用 `unsafe` 的地方：Windows 没有「安全的进程树句柄」，
/// 而「超时不留孤儿」是硬要求。`unsafe` 面被收在这个文件内部，外部只见安全的方法。
#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// 一个「句柄关闭即连坐整棵子树」的作业对象。
    pub(super) struct Job(HANDLE);

    impl Job {
        /// 建 Job 并把 `pid` 收进来；任何一步失败都返回 `None`，调用方退回 `taskkill`。
        pub(super) fn adopt(pid: u32) -> Option<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }

                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(info).cast(),
                    size,
                ) == 0
                {
                    CloseHandle(job);
                    return None;
                }

                // 进程可能已经在别的 Job 里（例如 CI 用 Job 管住整棵测试进程树）：
                // 这种情况下 assign 会失败，不是致命问题，退回 taskkill 路径。
                let handle = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if handle.is_null() {
                    CloseHandle(job);
                    return None;
                }
                let assigned = AssignProcessToJobObject(job, handle);
                CloseHandle(handle);
                if assigned == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }

        /// 终止作业里的全部进程。
        pub(super) fn terminate(&self) -> bool {
            unsafe { TerminateJobObject(self.0, 1) != 0 }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

// ---------------------------------------------------------------- 输出收集

/// 一条流收到的内容：只保留前 [`MAX_STREAM_BYTES`]，但总共读进来多少字节照实记。
struct Capture {
    kept: Vec<u8>,
    total: usize,
}

impl Capture {
    fn new() -> Self {
        Self {
            kept: Vec::new(),
            total: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        let room = MAX_STREAM_BYTES.saturating_sub(self.kept.len());
        if room > 0 {
            self.kept.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.kept).into_owned()
    }

    fn truncated(&self) -> bool {
        self.total > self.kept.len()
    }
}

/// 起一个读取线程把管道抽干（截断在 [`Capture::push`] 里做，读取本身不设限）。
fn spawn_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    sink: Arc<Mutex<Capture>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut buffer = [0u8; 8 * 1024];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let mut capture = sink.lock().unwrap_or_else(|err| err.into_inner());
                    capture.push(&buffer[..read]);
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

/// 等读取线程收尾；超时就返回 `false`——那说明有进程还攥着管道不放。
///
/// 这里**不能** `join()` 死等：杀掉直接子进程后，孙进程可能还活着并占着写端，
/// 死等会让整个工具调用挂住，比「输出拿不全」严重得多。
fn join_readers(readers: Vec<JoinHandle<()>>, grace: Duration) -> bool {
    let (done, wait) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for reader in readers {
            let _ = reader.join();
        }
        let _ = done.send(());
    });
    wait.recv_timeout(grace).is_ok()
}

/// 取出正文、真实总量、是否被截断。
fn snapshot(sink: &Arc<Mutex<Capture>>) -> (String, usize, bool) {
    let capture = sink.lock().unwrap_or_else(|err| err.into_inner());
    (capture.text(), capture.total, capture.truncated())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;
    use crate::testing::{call, Sandbox};
    use ai_ide_protocol::ErrorCode;
    use serde_json::Value;

    // ---------------------------------------------------------- 自举调用靶子
    //
    // 与其依赖宿主上恰好装着 `node` / `python`，不如让测试二进制**调用自己**：
    // `--ignored <靶子名>` 只跑指定的那条测试。跨平台、零外部依赖。
    // 靶子名都是唯一子串（libtest 的过滤器是子串匹配）。

    /// 靶子：分流写 stdout / stderr，并打出自己的工作目录，正常退出。
    #[test]
    #[ignore = "由 run_command 的测试按需唤起"]
    fn spawn_target_ok() {
        println!("OUT-MARKER");
        eprintln!("ERR-MARKER");
        println!("CWD={}", std::env::current_dir().unwrap().display());
    }

    /// 靶子：panic 退出（libtest 用 101 表示失败）。
    #[test]
    #[ignore = "由 run_command 的测试按需唤起"]
    fn spawn_target_fails() {
        panic!("这就是失败本身");
    }

    /// 靶子：往 stdout 灌 4 MiB，远超 64 KiB 的回传上限。
    #[test]
    #[ignore = "由 run_command 的测试按需唤起"]
    fn spawn_target_floods() {
        use std::io::Write;
        let chunk = "a".repeat(1024 * 1024);
        let mut out = std::io::stdout().lock();
        for _ in 0..4 {
            out.write_all(chunk.as_bytes()).unwrap();
        }
        out.flush().unwrap();
    }

    /// 靶子：派一个孙进程（同样攥着 stdout 管道），然后自己睡 30 秒。
    ///
    /// 这里**故意**不 `wait()` 那个孙进程：它的存活正是被测对象，
    /// 杀树测试要验证的就是父进程被超时处决后它也跟着没了。
    #[test]
    #[ignore = "由 run_command 的测试按需唤起"]
    #[allow(clippy::zombie_processes)]
    fn spawn_target_tree() {
        use std::io::Write;
        let exe = std::env::current_exe().expect("测试可执行文件");
        let child = Command::new(exe)
            .args(["--ignored", "--nocapture", "spawn_target_idle"])
            .spawn()
            .expect("派生孙进程");
        println!("GRANDCHILD-PID={}", child.id());
        std::io::stdout().flush().unwrap();
        thread::sleep(Duration::from_secs(30));
    }

    /// 孙进程：什么都不做，只是活着并攥着管道。
    #[test]
    #[ignore = "由 run_command 的测试按需唤起"]
    fn spawn_target_idle() {
        thread::sleep(Duration::from_secs(60));
    }

    // ---------------------------------------------------------- 辅助

    /// 唤起测试二进制自身：`target` 是靶子测试名的唯一子串。
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

    fn run(sandbox: &Sandbox, args: Value) -> Result<ToolOutcome, AgentError> {
        call(&RunCommandTool, sandbox, args)
    }

    fn result_of(outcome: &ToolOutcome) -> Value {
        serde_json::from_str(&outcome.content).expect("结果应是 JSON")
    }

    fn stdout_of(value: &Value) -> String {
        value["stdout"].as_str().unwrap_or_default().to_string()
    }

    // ---------------------------------------------------------- 正常路径

    #[test]
    fn runs_a_command_and_splits_the_streams() {
        let sandbox = Sandbox::new("run-ok");
        sandbox.write("sub/keep.txt", "x\n");
        let (exe, argv) = self_command("spawn_target_ok");

        let out = run(&sandbox, json!({"cmd": exe, "args": argv, "cwd": "sub"})).unwrap();
        let value = result_of(&out);

        assert_eq!(value["ok"], true);
        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["timed_out"], false);
        assert_eq!(value["killed_tree"], false);
        assert_eq!(value["signal"], Value::Null);
        assert_eq!(value["cwd"], "sub");

        let stdout = stdout_of(&value);
        let stderr = value["stderr"].as_str().unwrap();
        assert!(stdout.contains("OUT-MARKER"), "stdout 应收到标准输出");
        assert!(
            !stdout.contains("ERR-MARKER"),
            "stderr 不能混进 stdout：{stdout}"
        );
        assert!(stderr.contains("ERR-MARKER"), "stderr 应收到标准错误");
        assert!(!stderr.contains("OUT-MARKER"));

        assert_eq!(value["stdout_truncated"], false);
        assert_eq!(value["stderr_truncated"], false);
        assert!(
            value["stdout_bytes"].as_u64().unwrap() >= stdout.len() as u64,
            "字节数是真实总量的下界"
        );
        assert_eq!(
            value["streams_drained"], true,
            "读取线程必须在宽限期内收尾，否则说明管道还被人攥着"
        );

        // cwd 真的生效了（靶子自己打的当前目录）。
        let line = stdout
            .lines()
            .find(|line| line.starts_with("CWD="))
            .expect("靶子应打印工作目录");
        let tag = sandbox
            .path("")
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(line.contains(&tag), "工作目录应落在本沙箱里：{line}");
        assert!(line.ends_with("sub"), "cwd 参数应生效：{line}");
    }

    #[test]
    fn default_cwd_is_the_workspace_root() {
        let sandbox = Sandbox::new("run-default-cwd");
        let (exe, argv) = self_command("spawn_target_ok");

        let out = run(&sandbox, json!({"cmd": exe, "args": argv})).unwrap();
        let value = result_of(&out);
        assert_eq!(value["cwd"], ".");

        let stdout = stdout_of(&value);
        let line = stdout
            .lines()
            .find(|line| line.starts_with("CWD="))
            .expect("靶子应打印工作目录");
        let tag = sandbox
            .path("")
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            line.ends_with(&tag),
            "缺省工作目录应是工作区根：{line} 对不上 {tag}"
        );
    }

    #[test]
    fn non_zero_exit_is_reported_not_hidden() {
        let sandbox = Sandbox::new("run-fail");
        let (exe, argv) = self_command("spawn_target_fails");

        // 命令失败不是工具失败：结果仍要正常返回，让模型看见退出码。
        let out = run(&sandbox, json!({"cmd": exe, "args": argv})).unwrap();
        let value = result_of(&out);

        assert_eq!(value["ok"], false);
        assert_eq!(value["timed_out"], false);
        assert_eq!(
            value["exit_code"], 101,
            "libtest 用 101 表示测试失败，退出码要原样回传"
        );
    }

    // ---------------------------------------------------------- 超时与杀树

    #[cfg(windows)]
    #[test]
    fn times_out_and_kills_the_grandchildren_too() {
        let sandbox = Sandbox::new("run-tree");
        let (exe, argv) = self_command("spawn_target_tree");

        let started = Instant::now();
        let out = run(
            &sandbox,
            json!({"cmd": exe, "args": argv, "timeout_ms": 4000}),
        )
        .unwrap();
        let elapsed = started.elapsed();
        let value = result_of(&out);
        let stdout = stdout_of(&value);

        assert_eq!(value["timed_out"], true, "睡 30 秒的命令必须被判超时");
        assert_eq!(value["killed_tree"], true, "超时必须真的发出杀树动作");
        assert_ne!(value["exit_code"], 0, "被杀的进程不能报成正常退出");
        assert_eq!(value["ok"], false);
        assert!(
            value["streams_drained"].as_bool().unwrap(),
            "管道必须在宽限期内关闭：还有活着的孙进程攥着它"
        );
        assert!(
            elapsed >= Duration::from_millis(4000),
            "不该提前放弃，实际 {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(20),
            "杀完就该返回，不能挂在读线程上，实际 {elapsed:?}"
        );

        let grandchild: u32 = stdout
            .lines()
            .find_map(|line| line.strip_prefix("GRANDCHILD-PID="))
            .expect("靶子应打印孙进程 pid")
            .trim()
            .parse()
            .expect("pid 应是数字");
        assert!(
            !wait_until_dead(grandchild, Duration::from_secs(5)),
            "孙进程（pid {grandchild}）必须一起被清理，不能留孤儿"
        );
    }

    #[test]
    fn timeout_is_clamped_instead_of_becoming_unlimited() {
        let sandbox = Sandbox::new("run-timeout-clamp");
        let (exe, argv) = self_command("spawn_target_idle");

        // 0 会被夹到 1ms：等价于「立刻超时」，绝不能变成「永不超时」。
        let out = run(&sandbox, json!({"cmd": exe, "args": argv, "timeout_ms": 0})).unwrap();
        assert_eq!(
            result_of(&out)["timed_out"],
            true,
            "timeout_ms = 0 应被夹到最小值"
        );

        // 超大值被夹到上限，但仍能正常跑完（命令退得比上限早）。
        let (exe, argv) = self_command("spawn_target_ok");
        let out = run(
            &sandbox,
            json!({"cmd": exe, "args": argv, "timeout_ms": 999_999_999}),
        )
        .unwrap();
        assert_eq!(result_of(&out)["exit_code"], 0);
    }

    // ---------------------------------------------------------- 截断

    #[test]
    fn flooding_output_is_capped_but_the_real_size_is_reported() {
        let sandbox = Sandbox::new("run-flood");
        let (exe, argv) = self_command("spawn_target_floods");

        let out = run(&sandbox, json!({"cmd": exe, "args": argv})).unwrap();
        let value = result_of(&out);

        assert_eq!(value["exit_code"], 0, "灌数据不该把命令搞挂");
        assert_eq!(
            value["stdout_truncated"], true,
            "4 MiB 远超 64 KiB，必须标截断"
        );
        let total = value["stdout_bytes"].as_u64().unwrap();
        assert!(total >= 4 * 1024 * 1024, "真实总量要如实上报，实际 {total}");
        let kept = value["stdout"].as_str().unwrap().len();
        assert!(
            kept <= MAX_STREAM_BYTES,
            "回传正文不能超过上限，实际 {kept}"
        );
    }

    // ---------------------------------------------------------- 安全闸门

    #[test]
    fn malicious_commands_are_denied_before_spawning() {
        let sandbox = Sandbox::new("run-malicious");
        let cases: Vec<(Value, &str)> = vec![
            (json!({"cmd": "format", "args": ["C:"]}), "禁止执行的程序"),
            (
                json!({"cmd": "diskpart", "args": ["/s", "script.txt"]}),
                "禁止执行的程序",
            ),
            (
                json!({"cmd": "shutdown", "args": ["/r", "/t", "0"]}),
                "禁止执行的程序",
            ),
            (
                json!({"cmd": "reg", "args": ["delete", "HKLM\\Software\\X", "/f"]}),
                "reg delete",
            ),
            (
                json!({"cmd": "net", "args": ["user", "hacker", "p@ss", "/add"]}),
                "net user",
            ),
            (json!({"cmd": "rm", "args": ["-rf", "/"]}), "rm -rf"),
            (
                json!({"cmd": "C:\\Windows\\System32\\format.exe", "args": ["C:"]}),
                "禁止执行的程序",
            ),
        ];

        for (args, marker) in cases {
            let err = run(&sandbox, args.clone()).unwrap_err();
            assert!(err.is_fatal(), "{args} 必须终止循环，而不是回灌重试：{err}");
            assert_eq!(err.code(), ErrorCode::TOOL_NOT_ALLOWED, "{args}");
            assert_eq!(err.code().as_i32(), 3001);
            assert!(
                err.to_string().contains(marker),
                "{args} 的拒绝理由应点明 `{marker}`：{err}"
            );
        }
    }

    #[test]
    fn shell_wrapping_is_refused_with_a_hint() {
        let sandbox = Sandbox::new("run-shell");

        let err = run(&sandbox, json!({"cmd": "cmd", "args": ["/c", "echo hi"]})).unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert!(
            !err.is_fatal(),
            "这是用法错误、不是安全拒绝，回灌让模型改就好"
        );
        assert!(err.to_string().contains("不要用 shell 包装命令"), "{err}");

        let err = run(
            &sandbox,
            json!({"cmd": "git", "args": ["log", "--pretty=%H | head"]}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("shell 语法"), "{err}");

        let err = run(&sandbox, json!({"cmd": "echo", "args": ["hi > out.txt"]})).unwrap_err();
        assert!(err.to_string().contains("shell 语法"), "{err}");
    }

    #[test]
    fn cwd_must_stay_inside_the_workspace() {
        let sandbox = Sandbox::new("run-cwd-escape");

        let err = run(
            &sandbox,
            json!({"cmd": "x", "cwd": "../../Windows/System32"}),
        )
        .unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
        assert_eq!(err.code().as_i32(), 1002);

        std::fs::create_dir_all(sandbox.path(".git")).unwrap();
        let err = run(&sandbox, json!({"cmd": "x", "cwd": ".git"})).unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_DENIED);
        assert_eq!(err.code().as_i32(), 1003);
    }

    #[test]
    fn missing_program_is_recoverable_and_nothing_spawns() {
        let sandbox = Sandbox::new("run-noprogram");
        let err = run(
            &sandbox,
            json!({"cmd": "definitely-not-a-real-program-xyz"}),
        )
        .unwrap_err();
        assert!(matches!(err, AgentError::ToolFailed { .. }));
        assert!(!err.is_fatal());
        assert!(err.to_string().contains("无法启动"), "{err}");
    }

    #[test]
    fn missing_cmd_argument_is_bad_args() {
        let sandbox = Sandbox::new("run-noargs");
        let err = run(&sandbox, json!({"args": ["--version"]})).unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert!(!err.is_fatal());
    }

    #[test]
    fn missing_cwd_is_recoverable() {
        let sandbox = Sandbox::new("run-nocwd");
        let err = run(&sandbox, json!({"cmd": "cmd-x", "cwd": "nope"})).unwrap_err();
        assert!(matches!(err, AgentError::ToolFailed { .. }));
        assert!(!err.is_fatal());
        assert!(err.to_string().contains("工作目录不存在"), "{err}");
    }

    // ---------------------------------------------------------- 进程探针

    /// `pid` 在 `patience` 内是否还活着（Windows 用 `tasklist` 的 CSV 输出判断）。
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
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        let mut command = Command::new("tasklist");
        command
            .arg("/FI")
            .arg(format!("PID eq {pid}"))
            .arg("/FO")
            .arg("CSV")
            .arg("/NH")
            .stdin(Stdio::null());
        quiet(&mut command);
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
}
