//! 终端 crate 的单测：真起 PTY（Windows 走 ConPTY），不 mock。
//!
//! 这些用例会在测试进程里真的拉起 `cmd.exe` / `/bin/sh`，所以每个用例结束前都要
//! 把会话清干净；等待统一走 [`wait_for`]，避免 CI 上偶尔的慢启动把断言卡死。

use std::time::Duration;
use std::time::Instant;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::clamp_size;
use crate::SpawnOptions;
use crate::TerminalEvent;
use crate::TerminalManager;
use crate::DEFAULT_COLS;
use crate::DEFAULT_ROWS;
use crate::MAX_COLS;
use crate::MAX_ROWS;
use crate::MIN_COLS;
use crate::MIN_ROWS;

/// 单个用例等事件的宽限上限。ConPTY 起进程在冷机上偶尔要几秒。
const WAIT: Duration = Duration::from_secs(20);

/// 起一个「跑完就退」的 shell，退出码为 `code`。
fn exit_args(code: i32) -> Vec<String> {
    if cfg!(windows) {
        vec!["/c".to_string(), format!("exit {code}")]
    } else {
        vec!["-c".to_string(), format!("exit {code}")]
    }
}

/// 逐条收事件，直到 `predicate` 命中或超时。返回命中的那条。
fn wait_for<F>(rx: &mut UnboundedReceiver<TerminalEvent>, mut predicate: F) -> Option<TerminalEvent>
where
    F: FnMut(&TerminalEvent) -> bool,
{
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(event) => {
                if predicate(&event) {
                    return Some(event);
                }
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
        }
    }
    None
}

/// 把事件里的 `Data` 载荷解成可读文本（非法字节用 `?` 兜住，断言只看 ASCII 标记）。
fn text_of(event: &TerminalEvent) -> String {
    match event {
        TerminalEvent::Data { data_b64, .. } => {
            let bytes = BASE64.decode(data_b64).expect("事件里的 base64 必须能解码");
            String::from_utf8_lossy(&bytes).to_string()
        }
        TerminalEvent::Exit { .. } => String::new(),
    }
}

/// 收下一段输出，直到出现 `marker`（跨多个读取块的情况也要拼起来看）。
fn wait_for_text(rx: &mut UnboundedReceiver<TerminalEvent>, marker: &str) -> bool {
    let deadline = Instant::now() + WAIT;
    let mut seen = String::new();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(event) => {
                seen.push_str(&text_of(&event));
                if seen.contains(marker) {
                    return true;
                }
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return false,
        }
    }
    false
}

#[test]
fn clamp_size_keeps_values_inside_bounds() {
    assert_eq!(clamp_size(MIN_COLS - 1, 0), (MIN_COLS, MIN_ROWS));
    assert_eq!(clamp_size(MAX_COLS + 1, MAX_ROWS + 1), (MAX_COLS, MAX_ROWS));
    assert_eq!(clamp_size(120, 40), (120, 40));
}

#[test]
fn create_uses_default_size_and_lists_session() {
    let manager = TerminalManager::new();
    let info = manager
        .create(SpawnOptions::default())
        .expect("缺省选项应当能建出会话");

    assert_eq!(info.cols, DEFAULT_COLS);
    assert_eq!(info.rows, DEFAULT_ROWS);
    assert!(!info.id.is_empty());
    assert!(!info.shell.is_empty());
    assert!(info.created_at_ms > 0);

    let list = manager.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, info.id);
    assert_eq!(manager.count(), 1);
    assert!(manager.get(&info.id).is_some());

    manager.kill(&info.id).expect("kill 应当成功");
    assert_eq!(manager.count(), 0, "kill 之后会话必须从登记表摘掉");
    assert!(manager.get(&info.id).is_none());
}

#[test]
fn create_rejects_missing_cwd() {
    let manager = TerminalManager::new();
    let options = SpawnOptions {
        cwd: Some(std::path::PathBuf::from(
            "Z:\\definitely-not-a-real-dir\\ai-ide",
        )),
        ..SpawnOptions::default()
    };
    let err = manager
        .create(options)
        .expect_err("不存在的起始目录必须报错");
    assert!(!err.is_not_found());
    assert_eq!(manager.count(), 0, "建失败不能留下半个会话");
}

#[test]
fn interactive_shell_echoes_written_input() {
    let manager = TerminalManager::new();
    let mut rx = manager.take_events().expect("事件接收端只能取一次");
    let info = manager
        .create(SpawnOptions::default())
        .expect("建会话应当成功");

    // TTY 需要 `\r` 才算回车，`\n` 在 cmd.exe 里只换行不执行。
    manager
        .write(&info.id, b"echo ai-ide-pty-ok\r\n")
        .expect("写输入应当成功");

    assert!(
        wait_for_text(&mut rx, "ai-ide-pty-ok"),
        "没等到 shell 的回显输出"
    );

    manager.kill(&info.id).expect("kill 应当成功");
}

#[test]
fn resize_updates_view_and_clamps() {
    let manager = TerminalManager::new();
    let info = manager
        .create(SpawnOptions::default())
        .expect("建会话应当成功");

    let resized = manager.resize(&info.id, 120, 40).expect("resize 应当成功");
    assert_eq!((resized.cols, resized.rows), (120, 40));
    let view = manager.get(&info.id).expect("会话还在");
    assert_eq!((view.cols, view.rows), (120, 40), "list/get 要看到新尺寸");

    // 0 是窗口拖拽过程中的常态中间值，必须夹而不是报错。
    let clamped = manager
        .resize(&info.id, 0, 0)
        .expect("越界尺寸应当夹到合法区间");
    assert_eq!((clamped.cols, clamped.rows), (MIN_COLS, MIN_ROWS));

    manager.kill(&info.id).expect("kill 应当成功");
}

#[test]
fn process_exit_emits_event_and_cleans_up() {
    let manager = TerminalManager::new();
    let mut rx = manager.take_events().expect("事件接收端只能取一次");
    let options = SpawnOptions {
        args: exit_args(3),
        ..SpawnOptions::default()
    };
    let info = manager.create(options).expect("建会话应当成功");

    let event = wait_for(&mut rx, |event| {
        matches!(
            event,
            TerminalEvent::Exit { terminal_id, .. } if terminal_id == &info.id
        )
    })
    .expect("自然退出必须推 Exit 事件");

    match event {
        TerminalEvent::Exit { code, .. } => {
            assert_eq!(code, Some(3), "退出码要如实上报");
        }
        TerminalEvent::Data { .. } => unreachable!(),
    }

    // 退出事件发出时会话已经摘掉，`list` 不该再有它。
    let deadline = Instant::now() + WAIT;
    while manager.get(&info.id).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(manager.get(&info.id).is_none(), "退出后会话应自动清理");
    assert_eq!(manager.count(), 0);
}

#[test]
fn write_after_exit_reports_not_found() {
    let manager = TerminalManager::new();
    let mut rx = manager.take_events().expect("事件接收端只能取一次");
    let options = SpawnOptions {
        args: exit_args(0),
        ..SpawnOptions::default()
    };
    let info = manager.create(options).expect("建会话应当成功");

    wait_for(
        &mut rx,
        |event| matches!(event, TerminalEvent::Exit { terminal_id, .. } if terminal_id == &info.id),
    )
    .expect("应当收到退出事件");

    let err = manager
        .write(&info.id, b"echo too-late\r\n")
        .expect_err("已退出的会话不能再写");
    assert!(err.is_not_found(), "已退出会话应报 not found: {err}");
}

#[test]
fn unknown_session_operations_report_not_found() {
    let manager = TerminalManager::new();
    let missing = "00000000-0000-0000-0000-000000000000";

    assert!(manager.write(missing, b"x").unwrap_err().is_not_found());
    assert!(manager.resize(missing, 80, 24).unwrap_err().is_not_found());
    assert!(manager.kill(missing).unwrap_err().is_not_found());
    assert!(manager.get(missing).is_none());
    assert!(manager.list().is_empty());
}

#[test]
fn multiple_sessions_are_isolated() {
    let manager = TerminalManager::new();
    let mut rx = manager.take_events().expect("事件接收端只能取一次");

    let first = manager
        .create(SpawnOptions::default())
        .expect("第一个会话应当能建");
    let second = manager
        .create(SpawnOptions::default())
        .expect("第二个会话应当能建");
    assert_ne!(first.id, second.id);
    assert_eq!(manager.count(), 2);

    manager
        .write(&first.id, b"echo marker-one\r\n")
        .expect("写第一个会话");
    assert!(wait_for_text(&mut rx, "marker-one"), "第一个会话没有输出");

    manager
        .write(&second.id, b"echo marker-two\r\n")
        .expect("写第二个会话");
    assert!(wait_for_text(&mut rx, "marker-two"), "第二个会话没有输出");

    // 杀掉第一个，第二个必须活着——多会话不是「共享一个 PTY」。
    manager.kill(&first.id).expect("kill 第一个会话");
    assert_eq!(manager.count(), 1);
    assert!(manager.get(&second.id).is_some());
    assert!(manager.write(&first.id, b"x").unwrap_err().is_not_found());
    manager
        .write(&second.id, b"echo still-alive\r\n")
        .expect("第二个会话仍可写");
    assert!(wait_for_text(&mut rx, "still-alive"), "第二个会话被连坐");

    manager.shutdown_all();
    assert_eq!(manager.count(), 0);
}

#[test]
fn shutdown_all_closes_every_session() {
    let manager = TerminalManager::new();
    let mut rx = manager.take_events().expect("事件接收端只能取一次");

    let first = manager.create(SpawnOptions::default()).expect("建会话");
    let second = manager.create(SpawnOptions::default()).expect("建会话");
    assert_eq!(manager.count(), 2);

    manager.shutdown_all();
    assert_eq!(manager.count(), 0, "退出清理必须摘掉全部会话");
    assert!(manager.list().is_empty());
    assert!(manager.get(&first.id).is_none());
    assert!(manager.get(&second.id).is_none());

    // kill 也要推退出事件：客户端靠它关标签页，不能静默消失。
    WaitExit::new(&mut rx, &first.id).expect_exit();
    WaitExit::new(&mut rx, &second.id).expect_exit();
}

/// 小工具：只等某个会话的 `Exit`。
struct WaitExit<'a> {
    rx: &'a mut UnboundedReceiver<TerminalEvent>,
    id: &'a str,
}

impl<'a> WaitExit<'a> {
    fn new(rx: &'a mut UnboundedReceiver<TerminalEvent>, id: &'a str) -> Self {
        Self { rx, id }
    }

    fn expect_exit(self) {
        let id = self.id.to_string();
        let event = wait_for(
            self.rx,
            |event| matches!(event, TerminalEvent::Exit { terminal_id, .. } if terminal_id == &id),
        );
        assert!(event.is_some(), "会话 {id} 被 kill 后没有 Exit 事件");
    }
}

#[test]
fn events_carry_raw_bytes_not_lossy_text() {
    let manager = TerminalManager::new();
    let mut rx = manager.take_events().expect("事件接收端只能取一次");
    let info = manager
        .create(SpawnOptions::default())
        .expect("建会话应当成功");

    // 中文字节必须原样出现在 base64 里：先转 String 再推的话，半个字符就会被吞。
    manager
        .write(&info.id, "echo 终端字节流\r\n".as_bytes())
        .expect("写中文输入");

    let deadline = Instant::now() + WAIT;
    let mut decoded = Vec::new();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(event) => {
                if let TerminalEvent::Data {
                    data_b64, bytes, ..
                } = &event
                {
                    let raw = BASE64.decode(data_b64).expect("base64 必须合法");
                    assert_eq!(raw.len(), *bytes, "bytes 必须与解码后长度一致");
                    decoded.extend_from_slice(&raw);
                }
                if String::from_utf8_lossy(&decoded).contains("终端字节流") {
                    break;
                }
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
    assert!(
        String::from_utf8_lossy(&decoded).contains("终端字节流"),
        "中文回显丢失：base64 载荷不能走有损文本转换"
    );

    manager.kill(&info.id).expect("kill 应当成功");
}

#[test]
fn events_are_absent_until_receiver_is_taken() {
    // 没人取接收端时事件静默丢弃，但会话本身必须照常可用——这是单测与
    // 「客户端还没接上」两种场景的共同要求。
    let manager = TerminalManager::new();
    let info = manager
        .create(SpawnOptions::default())
        .expect("没有事件接收端也要能建会话");
    assert!(manager.write(&info.id, b"echo no-receiver\r\n").is_ok());
    assert!(manager.take_events().is_some());
    assert!(manager.take_events().is_none(), "接收端只能取一次");
    manager.kill(&info.id).expect("kill 应当成功");
}
