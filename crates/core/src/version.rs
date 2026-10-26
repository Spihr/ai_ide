//! 版本常量与进程级初始化。

/// 内核版本，取自 `Cargo.toml`。
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 内核实现的协议版本字面量。
pub const PROTOCOL_VERSION: &str = ai_ide_protocol::PROTOCOL_VERSION;

/// 初始化 tracing。
///
/// **关键约束**：日志写入 **stderr**。stdout 是协议通道，任何多余字节都会
/// 让主进程的行解析失败。
///
/// 日志级别通过环境变量 `AI_IDE_CORE_LOG` 控制，例如
/// `AI_IDE_CORE_LOG=debug` 或 `AI_IDE_CORE_LOG=ai_ide_core::dispatch=trace`。
///
/// **M5 任务 4**：在 stderr 之外再挂一条 JSONL 文件通道（见 [`crate::logging`]）。
/// 两条通道共用同一个 `EnvFilter`，因此级别开关只需要拧一次；文件通道按天滚动、
/// 出口脱敏，`AI_IDE_LOG_JSONL=off` 可整体关掉。
///
/// **P5 收口**：再挂一条 `core.log` 上报 layer（见 [`crate::core_log`]），把同一批
/// 事件抬成对外壳的单向通知。三条通道共用同一个 filter，脱敏口径也一致；
/// 上报本身可用 `AI_IDE_CORE_LOG_PUSH=off` 单独关掉，落盘不受影响。
/// layer 在这里挂、落点在 `run()` 里装：`init_tracing` 早于 `Peer` 构造，
/// 这段窗口里的日志只有 stderr 与文件两份，不会凭空消失。
pub fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("AI_IDE_CORE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(true);
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        // 第三条通道：把事件抬成 `core.log` 通知（落点在 `run()` 里装）。
        // 没装落点时这一层直接返回，不会比「没有这一层」多做什么。
        .with(crate::core_log::CoreLogLayer::default());

    if crate::logging::jsonl_enabled() {
        let writer = crate::logging::DailyJsonlWriter::new(crate::logging::log_dir());
        // span 列表不落文件：一行一条是「能 grep / 能被扫描器按行读」的前提。
        let json_layer = tracing_subscriber::fmt::layer()
            .event_format(crate::logging::JsonLinesFormatter)
            .with_writer(writer);
        let _ = registry.with(json_layer).try_init();
    } else {
        let _ = registry.try_init();
    }
}

/// 安装 panic 钩子。
///
/// panic 信息同样写 stderr；同时打上醒目前缀，便于主进程侧在日志里定位。
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[ai-ide-core][PANIC] {info}");
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_version_is_semver_like() {
        let parts: Vec<&str> = CORE_VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "版本号应为 x.y.z：{CORE_VERSION}");
    }

    #[test]
    fn protocol_version_parses() {
        assert!(ai_ide_protocol::ProtocolVersion::parse(PROTOCOL_VERSION).is_some());
    }
}
