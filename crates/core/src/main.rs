//! 内核二进制入口。
//!
//! 保持极薄——所有逻辑都在 lib 里，便于集成测试直接引用。

use anyhow::Result;

fn main() -> Result<()> {
    ai_ide_core::version::init_tracing();
    ai_ide_core::version::install_panic_hook();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ai-ide-core")
        .build()?;

    runtime.block_on(async {
        if let Err(err) = ai_ide_core::run().await {
            tracing::error!(error = %err, "内核异常退出");
            return Err(err);
        }
        Ok(())
    })
}
