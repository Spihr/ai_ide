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
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("AI_IDE_CORE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true)
        .try_init();
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
