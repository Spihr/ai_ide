//! 插件子进程装载器（P2）：拉起进程、按行帧协议、请求/响应与超时。
//!
//! 这一层只管「怎么和插件说话」，不认识清单、不碰账本、不判授权：
//!
//! - **帧协议**：一行一个 JSON 对象（JSON-RPC 2.0 风格），stdout 只走协议帧，
//!   日志走 stderr（由宿主收成 `plugin.log`）；
//! - **请求**：宿主发 `{jsonrpc,id,method,params}`，按 `id` 等回包，超时即
//!   [`RequestError::Timeout`]，绝不无限等；
//! - **插件反向请求**（`host/*`）：P3.5 起转给 [`HostApi`] 实现（读侧五个方法），
//!   宿主没接线或方法未实现时仍回 `-32601`。装成「成功」会让插件拿着假数据
//!   继续跑，比明确拒绝更难排查；
//! - **进程回收**：Windows 用 Job Object（`KILL_ON_JOB_CLOSE`）兜住「内核被
//!   强杀」这条路径——句柄随进程消失而关闭，整个作业连坐；正常关停先礼后兵
//!   （`plugin/shutdown` → 关 stdin → 超时 `kill`），最后再 terminate 作业。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::host_api::{HostApi, LOG_METHOD, PROGRESS_METHOD};
use crate::manifest::{HOST_PLUGIN_API_MAJOR, HOST_PLUGIN_API_MINOR};

/// 单帧上限：1 MiB。正常工具结果远小于此，超了就是内存放大攻击或写错管道。
pub const FRAME_LIMIT_BYTES: usize = 1024 * 1024;
/// 关停时留给插件自行退出的时间；过了就 `kill`。
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// stderr 只留尾部若干行：崩溃诊断要的是最后几行，不是整段日志。
const STDERR_TAIL_LINES: usize = 40;
/// 指定 Node 可执行文件的环境变量（自建发行版或 CI 里 node 不在 PATH 时用）。
pub const NODE_ENV: &str = "AIIDE_PLUGIN_NODE";

/// 插件回的错误帧（`error.code` / `error.message`）。
#[derive(Debug, Clone)]
pub struct ReplyError {
    pub code: i64,
    pub message: String,
}

/// 宿主自己塞给等待者的「进程没了」错误码。
///
/// 它必须和插件真的回的 JSON-RPC 错误帧区分开：插件回错是**确定性**的（代码问题），
/// 进程没人是**非确定性**的（值得自动重启）。两边都走 `Plugin` 分支的话，普通的崩溃
/// 会被误判成「插件拒绝握手」，自动重启就永远不会触发。
pub const EXITED_REPLY_CODE: i64 = -32000;

/// 一次请求的失败原因。调用方按它决定回哪个 61xx。
#[derive(Debug, Clone)]
pub enum RequestError {
    /// 插件明确回了错误帧。
    Plugin { code: i64, message: String },
    /// 等回包超时。
    Timeout { method: String, timeout: Duration },
    /// 进程已经没了（含读线程看到 EOF）。`tail` 是 stderr 尾巴，用于诊断。
    Exited { tail: Vec<String> },
    /// 帧不合协议（非法 JSON / 超长 / 不是对象）。
    Protocol(String),
    /// 管道已关，写不进去。
    Closed,
}

impl RequestError {
    /// stderr 尾巴（非退出类错误为空）。
    pub fn tail(&self) -> &[String] {
        match self {
            Self::Exited { tail } => tail,
            _ => &[],
        }
    }
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plugin { code, message } => write!(f, "插件回错误 {code}：{message}"),
            Self::Timeout { method, timeout } => {
                write!(f, "{method} 超时（{} ms）", timeout.as_millis())
            }
            Self::Exited { .. } => write!(f, "插件进程已退出"),
            Self::Protocol(message) => write!(f, "帧协议错误：{message}"),
            Self::Closed => write!(f, "插件输入管道已关闭"),
        }
    }
}

/// 拉起失败的两种情形。
#[derive(Debug, Clone)]
pub enum SpawnError {
    /// 入口文件不存在（装目录被外部动过）。
    MissingEntry(String),
    /// 找不到解释器（常见于用户机器没装 Node）。
    Interpreter { program: String, message: String },
    /// 其它 IO 错误。
    Io(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingEntry(path) => write!(f, "插件入口不存在：{path}"),
            Self::Interpreter { program, message } => {
                write!(
                    f,
                    "无法启动插件解释器 {program}：{message}（请确认已安装 Node.js 并在 PATH 中）"
                )
            }
            Self::Io(message) => write!(f, "启动插件进程失败：{message}"),
        }
    }
}

/// 起进程需要的最小信息。
pub struct SpawnOptions<'a> {
    pub plugin_id: &'a str,
    pub install_dir: &'a Path,
    pub entry: &'a Path,
    /// 宿主能力实现方（P3.5）。`None` 表示宿主没接线，`host/*` 一律回 -32601。
    pub host: Option<Arc<dyn HostApi>>,
}

/// 一次请求的应答：等到值，或等到错误。
type Reply = Result<Value, ReplyError>;

/// 在途请求表：请求 id → 等待者。读者线程拿到应答后从这里唤醒等待方。
type PendingReplies = Mutex<HashMap<u64, oneshot::Sender<Reply>>>;

/// 一个活着的插件子进程。
pub struct PluginProcess {
    id: String,
    pid: Option<u32>,
    child: AsyncMutex<Option<Child>>,
    stdin: Arc<AsyncMutex<Option<ChildStdin>>>,
    pending: Arc<PendingReplies>,
    next_id: AtomicU64,
    exited: Arc<AtomicBool>,
    /// 读线程判定协议违规时写进来的原因（只留第一条）。
    violation: Arc<Mutex<Option<String>>>,
    tail: Arc<Mutex<VecDeque<String>>>,
    /// 插件反向请求（`host/*`）的出口。`None` 表示宿主没接线（单测/精简构建）。
    host: Option<Arc<dyn HostApi>>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl std::fmt::Debug for PluginProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginProcess")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .field("exited", &self.exited.load(Ordering::SeqCst))
            .finish()
    }
}

/// 按扩展名选解释器：脚本走 Node，其余（原生可执行文件）直接执行。
fn interpreter_for(entry: &Path) -> Option<String> {
    let script = matches!(
        entry
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("mjs") | Some("js") | Some("cjs")
    );
    script.then(|| std::env::var(NODE_ENV).unwrap_or_else(|_| "node".to_string()))
}

impl PluginProcess {
    /// 拉起进程并开始泵 stdin/stderr。握手要调用方自己发。
    pub async fn spawn(options: SpawnOptions<'_>) -> Result<Arc<Self>, SpawnError> {
        let entry: PathBuf = options.entry.to_path_buf();
        if !entry.is_file() {
            return Err(SpawnError::MissingEntry(entry.display().to_string()));
        }

        let interpreter = interpreter_for(&entry);
        let program = interpreter
            .clone()
            .unwrap_or_else(|| entry.display().to_string());
        let mut command = Command::new(&program);
        if interpreter.is_some() {
            command.arg(&entry);
        }
        command
            .current_dir(options.install_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("AIIDE_PLUGIN_ID", options.plugin_id)
            .env(
                "AIIDE_HOST_API",
                format!("{HOST_PLUGIN_API_MAJOR}.{HOST_PLUGIN_API_MINOR}"),
            )
            // Child 被 drop 时兜一层：即便调用方漏了 shutdown，也不留下孤儿。
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // 不弹控制台窗口：插件是后台进程，黑框闪一下会让用户以为中毒。
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => SpawnError::Interpreter {
                program,
                message: error.to_string(),
            },
            _ => SpawnError::Io(error.to_string()),
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SpawnError::Io("拿不到 stdin 管道".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SpawnError::Io("拿不到 stdout 管道".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SpawnError::Io("拿不到 stderr 管道".to_string()))?;
        let pid = child.id();

        // 作业对象要在进程刚起来时收编：晚一步就可能漏掉它拉起的孙子进程。
        #[cfg(windows)]
        let job = match pid {
            Some(pid) => job::Job::adopt(pid),
            None => None,
        };

        let process = Arc::new(Self {
            id: options.plugin_id.to_string(),
            pid,
            child: AsyncMutex::new(Some(child)),
            stdin: Arc::new(AsyncMutex::new(Some(stdin))),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            exited: Arc::new(AtomicBool::new(false)),
            violation: Arc::new(Mutex::new(None)),
            tail: Arc::new(Mutex::new(VecDeque::new())),
            host: options.host,
            #[cfg(windows)]
            job,
        });

        tokio::spawn(pump_stdout(Arc::clone(&process), stdout));
        tokio::spawn(pump_stderr(Arc::clone(&process), stderr));
        tracing::info!(plugin_id = %process.id, pid = ?pid, "插件子进程已拉起");
        Ok(process)
    }

    /// 插件 id。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 宿主能力出口（未接线时为 `None`）。
    pub fn host(&self) -> Option<&Arc<dyn HostApi>> {
        self.host.as_ref()
    }

    /// 子进程 pid（拿不到时为 `None`）。
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// 进程是否已经退出（读线程看到 EOF 即置位）。
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// 协议违规原因（没有则为 `None`）。
    pub fn violation(&self) -> Option<String> {
        self.violation.lock().ok().and_then(|slot| slot.clone())
    }

    /// stderr 尾巴（最多 [`STDERR_TAIL_LINES`] 行）。
    pub fn stderr_tail(&self) -> Vec<String> {
        self.tail
            .lock()
            .map(|tail| tail.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 发一条请求并等回包。
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RequestError> {
        if self.has_exited() {
            return Err(RequestError::Exited {
                tail: self.stderr_tail(),
            });
        }
        if let Some(reason) = self.violation() {
            return Err(RequestError::Protocol(reason));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, sender);
        }
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.write_frame(&frame).await {
            self.forget(id);
            return Err(error);
        }

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(reply))) => Err(RequestError::Plugin {
                code: reply.code,
                message: reply.message,
            }),
            // 发送端被读线程丢弃：进程没了。
            Ok(Err(_)) => Err(RequestError::Exited {
                tail: self.stderr_tail(),
            }),
            Err(_) => {
                self.forget(id);
                Err(RequestError::Timeout {
                    method: method.to_string(),
                    timeout,
                })
            }
        }
    }

    /// 发一条**通知帧**（无 `id`，不等回包）——事件投递的唯一出口。
    ///
    /// 与 [`PluginProcess::request`] 的区别在于不等回包：事件是单向的，宿主
    /// 不能因为某个插件处理慢就卡住监听线程。写失败（进程已死、管道关了）
    /// 直接回 `Exited`，调用方按「这条事件算了」处理，不重试——插件死了以后
    /// 该由装载表的状态机去管，事件队列没有资格替它做决定。
    pub async fn notify(&self, method: &str, params: &Value) -> Result<(), RequestError> {
        if self.has_exited() {
            return Err(RequestError::Exited {
                tail: self.stderr_tail(),
            });
        }
        let frame = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_frame(&frame).await
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    async fn write_frame(&self, frame: &Value) -> Result<(), RequestError> {
        let mut line = serde_json::to_string(frame)
            .map_err(|error| RequestError::Protocol(format!("序列化请求失败：{error}")))?;
        line.push('\n');
        let mut slot = self.stdin.lock().await;
        let Some(pipe) = slot.as_mut() else {
            return Err(RequestError::Closed);
        };
        pipe.write_all(line.as_bytes())
            .await
            .map_err(|error| RequestError::Protocol(format!("写管道失败：{error}")))?;
        pipe.flush()
            .await
            .map_err(|error| RequestError::Protocol(format!("刷新管道失败：{error}")))
    }

    /// 等进程退出（轮询 `try_wait`）。返回 `Some(退出码)`；超时返回 `None`。
    pub async fn wait_exit(&self, timeout: Duration) -> Option<Option<i32>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let mut guard = self.child.lock().await;
                match guard.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => return Some(status.code()),
                        Ok(None) => {}
                        Err(_) => return Some(None),
                    },
                    None => return Some(None),
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 强杀子进程（不等待）。
    pub async fn kill(&self) {
        let mut guard = self.child.lock().await;
        if let Some(child) = guard.as_mut() {
            let _ = child.start_kill();
        }
    }

    /// 优雅关停：`plugin/shutdown` → 关 stdin → 超时强杀 → 清扫作业里的残留进程。
    pub async fn shutdown(&self) {
        if !self.has_exited() {
            // 支持的方法才有回包；不支持时插件会回 -32601，忽略即可。
            let _ = self
                .request("plugin/shutdown", json!({}), Duration::from_millis(1500))
                .await;
            self.close_stdin().await;
            if self.wait_exit(SHUTDOWN_GRACE).await.is_none() {
                tracing::warn!(plugin_id = %self.id, "插件未在宽限期内退出，强杀");
                self.kill().await;
                let _ = self.wait_exit(Duration::from_secs(2)).await;
            }
        }
        self.kill_descendants();
    }

    /// 关掉 stdin：正常实现的插件读到 EOF 就会自己退出。
    pub async fn close_stdin(&self) {
        let mut slot = self.stdin.lock().await;
        *slot = None;
    }

    /// 收掉作业里剩下的进程（插件自己拉起的孙子进程）。
    fn kill_descendants(&self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        // 同步上下文里拿不到 async 锁；`try_lock` 成功才动，失败就交给
        // `kill_on_drop` 与作业对象兜底。
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.start_kill();
            }
        }
        self.kill_descendants_sync();
    }
}

impl PluginProcess {
    fn kill_descendants_sync(&self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }
}

/// stdout 泵：逐行读帧、派发响应、把 `host/*` 请求转给宿主能力实现。
async fn pump_stdout(process: Arc<PluginProcess>, stdout: tokio::process::ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                if line.len() > FRAME_LIMIT_BYTES {
                    record_violation(
                        &process,
                        format!("单帧超过 {FRAME_LIMIT_BYTES} 字节（{}）", line.len()),
                    );
                    break;
                }
                let frame: Value = match serde_json::from_str(&line) {
                    Ok(frame) => frame,
                    Err(error) => {
                        record_violation(&process, format!("非法 JSON 帧：{error}"));
                        break;
                    }
                };
                if !frame.is_object() {
                    record_violation(&process, "帧不是 JSON 对象".to_string());
                    break;
                }
                if let Err(error) = dispatch_frame(&process, frame).await {
                    record_violation(&process, error);
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                record_violation(&process, format!("读 stdout 失败：{error}"));
                break;
            }
        }
    }

    process.exited.store(true, Ordering::SeqCst);
    let tail = process.stderr_tail();
    {
        let mut slot = process.stdin.lock().await;
        *slot = None;
    }
    if let Ok(mut pending) = process.pending.lock() {
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(ReplyError {
                code: EXITED_REPLY_CODE,
                message: "插件进程已退出".to_string(),
            }));
        }
    }
    tracing::debug!(plugin_id = %process.id, stderr_lines = tail.len(), "插件 stdout 已关闭");
}

/// 组装一条插件反向请求的响应帧。**纯函数**：不碰 IO、不碰装载表，便于单测。
///
/// 三道判定按顺序：
///
/// 1. 命名空间——只有 `host/*` 是插件能向宿主发起的；`plugin/*` 等反向调用属于
///    越权（docs/14 §8.2 T4），一律 `-32601`；
/// 2. 接线——宿主没装能力实现时如实回 `-32601`，绝不假装成功（假数据会让插件
///    带着错的假设继续跑，比明确拒绝更难排查）；
/// 3. 转发——错误码原样透传，闸门怎么判由实现方决定。
pub fn answer_plugin_request(
    host: Option<&Arc<dyn HostApi>>,
    plugin_id: &str,
    id: &Value,
    method: &str,
    params: &Value,
) -> Value {
    let error = |code: i64, message: String| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        })
    };

    if !method.starts_with("host/") {
        return error(
            -32601,
            format!("插件不能向宿主发起 {method}（可用命名空间只有 host/*）"),
        );
    }
    let Some(host) = host else {
        return error(-32601, format!("宿主能力尚未接线：{method}"));
    };

    match host.call(plugin_id, method, params) {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(failure) => error(failure.code, failure.message),
    }
}

/// 处理一帧：响应回填等待者；请求按命名空间放行；通知只走遥测白名单。
async fn dispatch_frame(process: &Arc<PluginProcess>, frame: Value) -> Result<(), String> {
    let id = frame.get("id").filter(|value| !value.is_null());
    let method = frame.get("method").and_then(Value::as_str);

    match (id, method) {
        // 响应帧：`{id, result}` 或 `{id, error}`。
        (Some(id), None) => {
            let Some(id) = id.as_u64() else {
                return Ok(()); // 插件自己的字符串 id（它发起请求时的回包）不归这里管
            };
            let outcome = match frame.get("error") {
                Some(error) => Err(ReplyError {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("插件未给出错误信息")
                        .to_string(),
                }),
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            };
            if let Ok(mut pending) = process.pending.lock() {
                if let Some(sender) = pending.remove(&id) {
                    let _ = sender.send(outcome);
                }
            }
            Ok(())
        }
        // 插件反向请求：`host/*` 交给宿主能力实现；其余命名空间仍是「方法不存在」。
        (Some(id), Some(method)) => {
            let params = frame
                .get("params")
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            let reply = answer_plugin_request(process.host(), process.id(), id, method, &params);
            process
                .write_frame(&reply)
                .await
                .map_err(|error| format!("回复 {method} 失败：{error}"))?;
            Ok(())
        }
        // 通知帧：只有 `host/log` / `host/progress` 被接受（P5 §6.5）。
        (None, Some(method)) => {
            let params = frame
                .get("params")
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            handle_notification(process, method, &params).await
        }
        (None, None) => Ok(()),
    }
}

/// 处理插件的通知帧（无 `id`，插件不等回包）。
///
/// 白名单只有两条，其余一律丢弃并记日志（没有 `id` 就没法回错帧）：
///
/// - `host/log`、`host/progress`：旁路遥测，交 [`HostApi::notify`] 转成
///   `plugin.log` / `plugin.progress` 事件，`pluginId` 由宿主填；
///   `host/progress` 另需 `progress` 能力，未授权由宿主拒——这里只丢这一帧，
///   不连坐：进度上报失败不该让插件停摆；
/// - `plugin/*`：越权（docs/14 §8.2 T4）。事件是宿主 → 插件单向的，
///   `plugin.state_changed` 尤其只能由宿主生成——插件冒用它就是伪造状态，
///   按协议违规处理（`Err` 会让帧泵记违规并停掉该进程），而不是静默忽略。
async fn handle_notification(
    process: &Arc<PluginProcess>,
    method: &str,
    params: &Value,
) -> Result<(), String> {
    if method.starts_with("plugin/") {
        return Err(format!(
            "插件不能发送 {method}（事件只允许宿主 → 插件单向投递）"
        ));
    }
    if method != LOG_METHOD && method != PROGRESS_METHOD {
        tracing::debug!(plugin_id = %process.id, %method, "忽略未知的插件通知");
        return Ok(());
    }
    let Some(host) = process.host() else {
        tracing::debug!(plugin_id = %process.id, %method, "宿主能力未接线，遥测丢弃");
        return Ok(());
    };
    // `pluginId` 由宿主盖：插件自己写一个只会被这里覆盖，遥测里的身份因此
    // 永远等于管道那一端的进程，冒名没有入口。
    let mut stamped = match params {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    stamped.insert(
        "pluginId".to_string(),
        Value::String(process.id().to_string()),
    );
    let stamped = Value::Object(stamped);
    match host.notify(process.id(), method, &stamped) {
        Ok(()) => Ok(()),
        Err(error) => {
            // 被拒只说明这一帧不被接受（`host/log` 是宿主没接线，`host/progress`
            // 是没这个能力）：丢一条遥测比停掉插件合理，所以不连坐。
            tracing::debug!(
                plugin_id = %process.id,
                %method,
                code = error.code,
                message = %error.message,
                "插件遥测被宿主拒绝"
            );
            Ok(())
        }
    }
}

fn record_violation(process: &PluginProcess, reason: String) {
    tracing::warn!(plugin_id = %process.id, %reason, "插件帧协议违规");
    if let Ok(mut slot) = process.violation.lock() {
        if slot.is_none() {
            *slot = Some(reason);
        }
    }
}

/// stderr 泵：只保留尾部若干行（崩溃时用来填 `last_error`）。
async fn pump_stderr(process: Arc<PluginProcess>, stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(plugin_id = %process.id, %line, "插件 stderr");
        if let Ok(mut tail) = process.tail.lock() {
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
}

// ---------------------------------------------------------------- 作业对象

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

    /// 「句柄关闭即连坐整棵进程树」的作业对象。
    ///
    /// 这是「内核被任务管理器强杀后插件不残留」的唯一可靠手段：强杀时没有任何
    /// 用户态代码能跑，但操作系统会关闭进程的全部句柄，作业句柄一关，
    /// `KILL_ON_JOB_CLOSE` 就替我们把插件杀掉。
    pub(super) struct Job(HANDLE);

    // 句柄只是个内核编号，任何线程都能关它、终结它，所以 Job 本身线程安全。
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
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

                // 进程可能已经在别的作业里（例如 CI 用作业管住整棵测试进程树）：
                // assign 失败不致命，退回「关停时 kill 子进程 + 作业句柄关闭」。
                let handle = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if handle.is_null() {
                    CloseHandle(job);
                    return None;
                }
                let assigned = AssignProcessToJobObject(job, handle);
                CloseHandle(handle);
                if assigned == 0 {
                    tracing::warn!(pid, "把插件进程收进作业对象失败");
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn node_ready() -> bool {
        std::process::Command::new(std::env::var(NODE_ENV).unwrap_or_else(|_| "node".to_string()))
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn scratch(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|gap| gap.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aiide-plugin-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("写脚本");
        path
    }

    #[test]
    fn scripts_get_node_and_binaries_run_directly() {
        assert!(interpreter_for(Path::new("a/index.mjs")).is_some());
        assert!(interpreter_for(Path::new("a/index.js")).is_some());
        assert!(interpreter_for(Path::new("a/index.cjs")).is_some());
        assert!(interpreter_for(Path::new("a/plugin.exe")).is_none());
        assert!(interpreter_for(Path::new("a/noext")).is_none());
    }

    /// 最小的协议替身：收到任何请求就回 `{ok:true, method}`。
    const ECHO: &str = r#"
        const readline = require('node:readline')
        const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
        rl.on('line', (line) => {
          if (!line.trim()) return
          const frame = JSON.parse(line)
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: frame.id, result: { ok: true, method: frame.method }
          }) + '\n')
        })
    "#;

    #[tokio::test]
    async fn request_round_trip_and_clean_shutdown() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("round-trip");
        let entry = write_script(&dir, "index.cjs", ECHO);
        let process = PluginProcess::spawn(SpawnOptions {
            plugin_id: "demo",
            install_dir: &dir,
            entry: &entry,
            host: None,
        })
        .await
        .expect("起进程");

        let reply = process
            .request("plugin/ping", json!({}), Duration::from_secs(5))
            .await
            .expect("回包");
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["method"], json!("plugin/ping"));

        process.shutdown().await;
        assert!(process.has_exited(), "关停后进程应已退出");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn plugin_error_frames_surface_as_plugin_errors() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("error-frame");
        let body = r#"
            const readline = require('node:readline')
            const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
            rl.on('line', (line) => {
              if (!line.trim()) return
              const frame = JSON.parse(line)
              process.stdout.write(JSON.stringify({
                jsonrpc: '2.0', id: frame.id,
                error: { code: -32601, message: '未实现的方法：' + frame.method }
              }) + '\n')
            })
        "#;
        let entry = write_script(&dir, "index.cjs", body);
        let process = PluginProcess::spawn(SpawnOptions {
            plugin_id: "demo",
            install_dir: &dir,
            entry: &entry,
            host: None,
        })
        .await
        .expect("起进程");

        match process
            .request("plugin/handshake", json!({}), Duration::from_secs(5))
            .await
        {
            Err(RequestError::Plugin { code, message }) => {
                assert_eq!(code, -32601);
                assert!(message.contains("未实现"), "{message}");
            }
            other => panic!("应收到插件错误帧，实际 {other:?}"),
        }
        process.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dead_process_reports_exited_with_stderr_tail() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("crash");
        let entry = write_script(
            &dir,
            "index.cjs",
            "process.stderr.write('boom: 启动即崩\\n'); process.exit(3)",
        );
        let process = PluginProcess::spawn(SpawnOptions {
            plugin_id: "demo",
            install_dir: &dir,
            entry: &entry,
            host: None,
        })
        .await
        .expect("起进程");

        // 等读线程看到 EOF：崩溃的进程没有回包，只能靠退出信号。
        let mut exited = false;
        for _ in 0..100 {
            if process.has_exited() {
                exited = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(exited, "进程应立即退出");
        match process
            .request("plugin/ping", json!({}), Duration::from_millis(200))
            .await
        {
            Err(RequestError::Exited { tail }) => {
                assert!(
                    tail.iter().any(|line| line.contains("boom")),
                    "退出时应带上 stderr 尾巴：{tail:?}"
                );
            }
            other => panic!("应报进程已退出，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn timeout_is_bounded_and_reported() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("timeout");
        // 读完一行就装死：不回包、不退出。
        let entry = write_script(
            &dir,
            "index.cjs",
            "process.stdin.once('data', () => {}); setInterval(() => {}, 1000)",
        );
        let process = PluginProcess::spawn(SpawnOptions {
            plugin_id: "demo",
            install_dir: &dir,
            entry: &entry,
            host: None,
        })
        .await
        .expect("起进程");

        let started = std::time::Instant::now();
        match process
            .request("plugin/handshake", json!({}), Duration::from_millis(300))
            .await
        {
            Err(RequestError::Timeout { timeout, .. }) => {
                assert_eq!(timeout, Duration::from_millis(300));
            }
            other => panic!("应超时，实际 {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "超时必须是硬上限，不能拖"
        );
        process.shutdown().await;
        assert!(process.has_exited(), "关停应杀掉装死的插件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 假的宿主能力实现：`host/fs.read` 回显路径，`host/boom` 报 6109。
    struct FakeHost;

    impl HostApi for FakeHost {
        fn call(
            &self,
            plugin_id: &str,
            method: &str,
            params: &Value,
        ) -> Result<Value, crate::host_api::HostApiError> {
            if method == "host/boom" {
                return Err(crate::host_api::HostApiError::new(6109, "配额用尽"));
            }
            Ok(json!({ "pluginId": plugin_id, "path": params["path"] }))
        }
    }

    #[test]
    fn host_request_is_forwarded_to_api() {
        let api: Arc<dyn HostApi> = Arc::new(FakeHost);
        let reply = answer_plugin_request(
            Some(&api),
            "demo",
            &json!(7),
            "host/fs.read",
            &json!({ "path": "src/main.rs" }),
        );
        assert_eq!(reply["id"], json!(7));
        assert_eq!(reply["result"]["pluginId"], json!("demo"));
        assert_eq!(reply["result"]["path"], json!("src/main.rs"));
        assert!(reply.get("error").is_none());
    }

    #[test]
    fn host_request_without_api_is_method_not_found() {
        let reply = answer_plugin_request(
            None,
            "demo",
            &json!(1),
            "host/fs.read",
            &json!({ "path": "a.txt" }),
        );
        assert_eq!(reply["error"]["code"], json!(-32601));
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("宿主能力尚未接线"));
    }

    #[test]
    fn non_host_namespace_is_rejected() {
        let api: Arc<dyn HostApi> = Arc::new(FakeHost);
        let reply = answer_plugin_request(
            Some(&api),
            "demo",
            &json!("abc"),
            "plugin/unload",
            &json!({}),
        );
        assert_eq!(reply["error"]["code"], json!(-32601));
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("host/*"));
    }

    #[test]
    fn api_error_passes_through_code_and_message() {
        let api: Arc<dyn HostApi> = Arc::new(FakeHost);
        let reply = answer_plugin_request(Some(&api), "demo", &json!(3), "host/boom", &json!({}));
        assert_eq!(reply["error"]["code"], json!(6109));
        assert_eq!(reply["error"]["message"], json!("配额用尽"));
        assert!(reply.get("result").is_none());
    }
}
