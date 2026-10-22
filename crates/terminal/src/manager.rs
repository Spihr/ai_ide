//! 会话管理器：建会话、写输入、改尺寸、杀掉、列清单，并把输出抬成事件流。
//!
//! # 线程模型
//!
//! PTY 的读写口是**阻塞**的（Windows 的 ConPTY 管道不支持 IOCP 异步读），所以每个
//! 会话各起两个后台线程：
//!
//! - `pty-read-<id>`：`read` 到底就 `break`，把每块原始字节 base64 后推给事件通道；
//! - `pty-wait-<id>`：`wait` 子进程，拿到退出码后登记退出信息、把会话从登记表摘掉，
//!   再推一条 `Exit` 事件。
//!
//! 事件通道是**单一**的（整进程一条），由 `crates/core` 起一个任务读出来再转成
//! JSON-RPC 通知。这么做的好处是客户端看到的顺序与内核产生事件的顺序一致；
//! 每个会话各配一条通道反而要在客户端侧重新排序。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use portable_pty::native_pty_system;
use portable_pty::Child;
use portable_pty::ChildKiller;
use portable_pty::CommandBuilder;
use portable_pty::MasterPty;
use portable_pty::PtySize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;
use tracing::warn;
use uuid::Uuid;

use crate::error::TerminalError;
use crate::types::SpawnOptions;
use crate::types::TerminalEvent;
use crate::types::TerminalInfo;

/// 单次 `read` 的缓冲上限。8 KiB 是经验值：再小会让 `cargo build` 这类刷屏输出
/// 拆成上千条通知，再大则首屏延迟明显。
const READ_CHUNK: usize = 8 * 1024;

/// ConPTY 起步时会向「终端」发一条光标位置查询（DSR，`ESC [ 6 n`），
/// 拿到回复之前它不会把子进程的输出转出来。真实终端都会回这一句，
/// 我们既然扮演终端，就得回——否则前端看到的是一个永远空白的终端
/// （Windows 10 24H2 之后此行为更明显：不回连 shell 横幅都出不来）。
const DSR_QUERY: &[u8] = b"\x1b[6n";

/// 光标位置回复：第 1 行第 1 列。ConPTY 只拿它当「终端已就绪」的信号，
/// 具体行列并不参与渲染（真正的绘制在客户端）。
const DSR_REPLY: &[u8] = b"\x1b[1;1R";

/// `kill` 之后等子进程退出的上限。超时就当它已经死了——阻塞在这里比漏一条
/// 退出事件更糟，客户端还有 `terminal.list` 可以自查。
const KILL_GRACE: Duration = Duration::from_secs(3);

/// 缺省 shell（Windows 取 `%COMSPEC%`，Unix 取 `$SHELL`，都取不到时用它）。
#[cfg(windows)]
const FALLBACK_SHELL: &str = "cmd.exe";
#[cfg(not(windows))]
const FALLBACK_SHELL: &str = "/bin/sh";

/// 从哪个环境变量里找缺省 shell。
#[cfg(windows)]
const SHELL_ENV: &str = "COMSPEC";
#[cfg(not(windows))]
const SHELL_ENV: &str = "SHELL";

/// 子进程的退出信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    /// 退出码；被信号杀死时由平台给一个约定值（Windows 为终止码）。
    pub code: Option<i32>,
    /// 终止信号名（Unix）；Windows 恒为 `None`。
    pub signal: Option<String>,
}

/// 会话登记表 + 事件出口。整个进程一份，`Arc` 共享给 `AppState`。
pub struct TerminalManager {
    shared: Arc<Shared>,
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 内部共享状态：会话表与事件发送端。
struct Shared {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// 事件发送端。`crates/core` 通过 [`TerminalManager::take_events`] 取走接收端；
    /// 单测里不取，事件静默丢弃（发送失败忽略即可）。
    events: Mutex<Option<UnboundedSender<TerminalEvent>>>,
}

impl Shared {
    /// 推一条事件。没有接收端时静默丢弃，绝不 panic——后台线程里 panic 会把
    /// 整个进程的 stderr 弄脏，还会让会话永远停在「有 pid 但不回应」的状态。
    fn emit(&self, event: TerminalEvent) {
        let guard = match self.events.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(tx) = guard.as_ref() {
            if tx.send(event).is_err() {
                debug!("终端事件接收端已关闭，事件丢弃");
            }
        }
    }

    fn get(&self, id: &str) -> Option<Arc<Session>> {
        let table = match self.sessions.lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        table.get(id).map(Arc::clone)
    }

    fn insert(&self, session: Arc<Session>) {
        let id = session.id().to_string();
        let mut table = match self.sessions.lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        table.insert(id, session);
    }

    fn remove(&self, id: &str) -> Option<Arc<Session>> {
        let mut table = match self.sessions.lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        table.remove(id)
    }

    fn snapshot(&self) -> Vec<TerminalInfo> {
        let table = match self.sessions.lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        table.values().map(|session| session.info()).collect()
    }

    fn count(&self) -> usize {
        let table = match self.sessions.lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        table.len()
    }
}

/// 一个活着的终端会话。
struct Session {
    /// 当前视图（`resize` 会更新 `cols` / `rows`，所以整份放在锁里）。
    info: Mutex<TerminalInfo>,
    /// 输入口：`terminal.write` 的载荷原样灌进去。
    writer: Mutex<Box<dyn Write + Send>>,
    /// 主端句柄，`resize` 用。ConPTY 的 resize 必须走这里，改子进程的
    /// `TerminalInfo` 是不够的——真正的行列数是系统给的。
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// 子进程杀手。留着它是因为 `Child` 本体已经搬进等待线程了。
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// 退出信息。`Some` 之前一直 `None`，配合 `exit_signal` 做「等它死」。
    exit: Mutex<Option<ExitInfo>>,
    /// 退出通知。判活、`kill` 后的收尾都靠它，避免轮询。
    exit_signal: Condvar,
}

impl Session {
    fn id(&self) -> String {
        self.info
            .lock()
            .map(|info| info.id.clone())
            .unwrap_or_default()
    }

    fn info(&self) -> TerminalInfo {
        match self.info.lock() {
            Ok(info) => info.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// 回一条光标位置报告（见 [`DSR_QUERY`]）。
    ///
    /// 失败可以忽略：会话可能刚好退出，或者写端口已经关了。这里是「尽力而为」，
    /// 因为不回复的后果只是输出晚一点，而报错打断读线程会让会话彻底哑掉。
    fn reply_dsr(&self) {
        let mut writer = match self.writer.lock() {
            Ok(writer) => writer,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(err) = writer.write_all(DSR_REPLY).and_then(|()| writer.flush()) {
            debug!(error = %err, "回光标位置报告失败");
        }
    }

    fn mark_exited(&self, exit: ExitInfo) {
        let mut slot = match self.exit.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = Some(exit);
        self.exit_signal.notify_all();
    }

    fn exit_info(&self) -> Option<ExitInfo> {
        match self.exit.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// 等子进程退出，最多等 `timeout`。返回它是否真的退了。
    fn wait_exit(&self, timeout: Duration) -> bool {
        let slot = match self.exit.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot.is_some() {
            return true;
        }
        let (slot, _timeout) = match self.exit_signal.wait_timeout(slot, timeout) {
            Ok(pair) => pair,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.is_some()
    }
}

impl TerminalManager {
    /// 建一个管理器并建好事件通道。
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                sessions: Mutex::new(HashMap::new()),
                events: Mutex::new(None),
            }),
        }
    }

    /// 取走事件接收端（一个进程只该取一次）。
    ///
    /// 取走之前产生的事件会丢——所以 `crates/core` 必须在开始读 stdin **之前**调用，
    /// 那时还没有任何客户端能发 `terminal.create`。
    pub fn take_events(&self) -> Option<UnboundedReceiver<TerminalEvent>> {
        let mut guard = match self.shared.events.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.is_some() {
            return None;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        *guard = Some(tx);
        Some(rx)
    }

    /// 起一个新会话。
    pub fn create(&self, options: SpawnOptions) -> Result<TerminalInfo, TerminalError> {
        let shell = resolve_shell(options.shell.as_deref())?;
        let (cols, rows) = options.clamped_size();

        if let Some(cwd) = options.cwd.as_ref() {
            if !cwd.is_dir() {
                return Err(TerminalError::Invalid(format!(
                    "起始目录不存在或不是目录: {}",
                    cwd.display()
                )));
            }
        }

        let mut builder = CommandBuilder::new(&shell);
        for arg in &options.args {
            builder.arg(arg);
        }
        if let Some(cwd) = options.cwd.as_ref() {
            builder.cwd(cwd);
        }
        for (key, value) in &options.env {
            builder.env(key, value);
        }
        // 让 shell 知道自己在一个 256 色终端里：否则 `ls` / `git` 会关掉颜色，
        // 客户端那边的 ANSI 渲染器就没东西可渲染了。
        builder.env("TERM", "xterm-256color");

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| TerminalError::Spawn(format!("建伪终端失败: {err}")))?;

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|err| TerminalError::Spawn(format!("启动 {shell} 失败: {err}")))?;

        let pid = child.process_id();
        let killer = child.clone_killer();
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| TerminalError::Io(format!("取写入口失败: {err}")))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| TerminalError::Io(format!("取读出口失败: {err}")))?;
        // 从端必须尽早丢掉：它跟着父进程活着的话，master 永远等不到 EOF，
        // 读线程就会一直挂着。
        drop(pair.slave);

        let id = Uuid::new_v4().to_string();
        let info = TerminalInfo {
            id: id.clone(),
            shell: shell.clone(),
            cwd: options
                .cwd
                .as_ref()
                .map(|cwd| cwd.to_string_lossy().to_string()),
            pid,
            cols,
            rows,
            created_at_ms: now_ms(),
        };

        let session = Arc::new(Session {
            info: Mutex::new(info.clone()),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            exit: Mutex::new(None),
            exit_signal: Condvar::new(),
        });
        self.shared.insert(Arc::clone(&session));

        spawn_reader(Arc::clone(&self.shared), Arc::clone(&session), reader).map_err(|err| {
            self.shared.remove(&id);
            let _ = session.killer.lock().map(|mut killer| killer.kill().ok());
            TerminalError::Spawn(format!("起读线程失败: {err}"))
        })?;

        spawn_waiter(Arc::clone(&self.shared), Arc::clone(&session), child).map_err(|err| {
            self.shared.remove(&id);
            let _ = session.killer.lock().map(|mut killer| killer.kill().ok());
            TerminalError::Spawn(format!("起等待线程失败: {err}"))
        })?;

        debug!(terminal_id = %id, shell = %shell, pid = ?pid, "已建终端会话");
        Ok(info)
    }

    /// 往会话里写输入。返回写入的字节数。
    ///
    /// 载荷是**原始字节**：换行要不要转 `\r` 由调用方决定（TTY 需要 `\r`
    /// 才会触发回车），内核不替谁猜。
    pub fn write(&self, id: &str, data: &[u8]) -> Result<usize, TerminalError> {
        let session = self.shared.get(id);
        let session = session.ok_or_else(|| TerminalError::NotFound(id.to_string()))?;

        if let Some(exit) = session.exit_info() {
            return Err(TerminalError::NotFound(format!(
                "{id}（进程已退出，代码 {:?}）",
                exit.code
            )));
        }

        let mut writer = match session.writer.lock() {
            Ok(writer) => writer,
            Err(poisoned) => poisoned.into_inner(),
        };
        writer
            .write_all(data)
            .and_then(|()| writer.flush())
            .map_err(|err| TerminalError::Io(format!("写终端失败: {err}")))?;
        Ok(data.len())
    }

    /// 改会话尺寸。越界值夹到合法区间后照做。
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<TerminalInfo, TerminalError> {
        let session = self
            .shared
            .get(id)
            .ok_or_else(|| TerminalError::NotFound(id.to_string()))?;
        let (cols, rows) = crate::error::clamp_size(cols, rows);

        {
            let master = match session.master.lock() {
                Ok(master) => master,
                Err(poisoned) => poisoned.into_inner(),
            };
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|err| TerminalError::Io(format!("改尺寸失败: {err}")))?;
        }

        let mut info = match session.info.lock() {
            Ok(info) => info,
            Err(poisoned) => poisoned.into_inner(),
        };
        info.cols = cols;
        info.rows = rows;
        Ok(info.clone())
    }

    /// 杀掉会话。子进程退出事件照常推（客户端据此清掉标签页），返回前保证
    /// 会话已从登记表摘掉。
    pub fn kill(&self, id: &str) -> Result<(), TerminalError> {
        let session = self
            .shared
            .get(id)
            .ok_or_else(|| TerminalError::NotFound(id.to_string()))?;

        {
            let mut killer = match session.killer.lock() {
                Ok(killer) => killer,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Err(err) = killer.kill() {
                // 进程可能已经自己退了：这不是错误，继续往下等退出信息。
                debug!(terminal_id = %id, error = %err, "发送终止信号失败，按已退出处理");
            }
        }

        if !session.wait_exit(KILL_GRACE) {
            warn!(terminal_id = %id, "终止后未在宽限期内退出");
        }
        self.shared.remove(id);
        Ok(())
    }

    /// 列出全部会话，按建会话时间升序（同刻按 id 兜底，保证顺序稳定）。
    pub fn list(&self) -> Vec<TerminalInfo> {
        let mut sessions = self.shared.snapshot();
        sessions.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        sessions
    }

    /// 当前会话数。
    pub fn count(&self) -> usize {
        self.shared.count()
    }

    /// 取会话视图。
    pub fn get(&self, id: &str) -> Option<TerminalInfo> {
        self.shared.get(id).map(|session| session.info())
    }

    /// 停掉全部会话。内核退出前调用：不这么做的话，ConPTY 里的
    /// `cmd.exe` 会变成孤儿进程挂在任务管理器里。
    pub fn shutdown_all(&self) {
        for info in self.list() {
            if let Err(err) = self.kill(&info.id) {
                debug!(terminal_id = %info.id, error = %err, "退出时清理终端失败");
            }
        }
    }
}

/// 这一块（连同上一次的尾巴）里有没有光标位置查询。
fn contains_dsr(carry: &[u8], chunk: &[u8]) -> bool {
    let mut window = Vec::with_capacity(carry.len() + chunk.len());
    window.extend_from_slice(carry);
    window.extend_from_slice(chunk);
    window
        .windows(DSR_QUERY.len())
        .any(|window| window == DSR_QUERY)
}

/// 起读线程：把 master 的原始字节抬成 `Data` 事件。
fn spawn_reader(
    shared: Arc<Shared>,
    session: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
) -> std::io::Result<()> {
    let id = session.id();
    std::thread::Builder::new()
        .name(format!("pty-read-{}", short(&id)))
        .spawn(move || {
            let mut buffer = vec![0u8; READ_CHUNK];
            // 上一块末尾的几字节：DSR 只有 4 字节，但仍可能被切在两次读取之间。
            let mut carry: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let chunk = &buffer[..read];
                        if contains_dsr(&carry, chunk) {
                            session.reply_dsr();
                        }
                        carry.clear();
                        let keep = chunk.len().min(DSR_QUERY.len() - 1);
                        carry.extend_from_slice(&chunk[chunk.len() - keep..]);
                        shared.emit(TerminalEvent::Data {
                            terminal_id: id.clone(),
                            data_b64: BASE64.encode(chunk),
                            bytes: read,
                        });
                    }
                    Err(err) => {
                        // 会话被 kill 时管道会先断，`read` 报错属于预期路径。
                        debug!(terminal_id = %id, error = %err, "终端读取结束");
                        break;
                    }
                }
            }
        })
        .map(|_| ())
}

/// 起等待线程：子进程退出 → 登记退出信息 → 摘会话 → 推 `Exit` 事件。
fn spawn_waiter(
    shared: Arc<Shared>,
    session: Arc<Session>,
    mut child: Box<dyn Child + Send + Sync>,
) -> std::io::Result<()> {
    let id = session.id();
    std::thread::Builder::new()
        .name(format!("pty-wait-{}", short(&id)))
        .spawn(move || {
            let exit = match child.wait() {
                Ok(status) => ExitInfo {
                    code: Some(status.exit_code() as i32),
                    signal: status.signal().map(|signal| signal.to_string()),
                },
                Err(err) => {
                    debug!(terminal_id = %id, error = %err, "等待子进程退出失败");
                    ExitInfo {
                        code: None,
                        signal: None,
                    }
                }
            };
            session.mark_exited(exit.clone());
            shared.remove(&id);
            shared.emit(TerminalEvent::Exit {
                terminal_id: id.clone(),
                code: exit.code,
                signal: exit.signal,
                at_ms: now_ms(),
            });
        })
        .map(|_| ())
}

/// 取 shell：显式给了就用给的，否则走平台缺省。
fn resolve_shell(shell: Option<&str>) -> Result<String, TerminalError> {
    if let Some(shell) = shell.map(str::trim).filter(|shell| !shell.is_empty()) {
        return Ok(shell.to_string());
    }
    let from_env = std::env::var(SHELL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(from_env.unwrap_or_else(|| FALLBACK_SHELL.to_string()))
}

/// 线程名里只放前 8 位：完整 UUID 会让某些平台的线程名截断成不可读的一坨。
fn short(id: &str) -> &str {
    let end = id
        .char_indices()
        .nth(8)
        .map(|(idx, _)| idx)
        .unwrap_or(id.len());
    &id[..end]
}

/// 当前 Unix 毫秒时间戳。系统时钟异常时回退为 `0`。
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
