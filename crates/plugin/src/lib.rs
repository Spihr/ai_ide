//! 插件系统的内核侧实现。
//!
//! 两片职责，边界清晰：
//!
//! - **P1 安装**：[`manifest`] 做清单三段校验（形状 → 语义 → 交叉），[`install`]
//!   负责「把包安全地拷进 `<data_dir>/plugins/<id>`」。安装全程不起进程。
//! - **P2 装载**：[`loader`] 拉起子进程并实现按行帧协议（请求/响应/超时/stderr
//!   尾巴），[`host`] 在其上做握手与一致性校验（`pluginApi` + 工具集合），并维
//!   护「已装载插件表」——崩溃即下架，卸载即关停。
//!
//! 这一层不认识账本：状态该落 `installed` / `needs_grant` / `ready` / `crashed`
//! 由 `crates/core` 决定，插件 crate 只如实回答「进程在不在」「为什么死的」。
//!
//! 进程回收有三道防线，对应 docs/14 §6.5：
//!
//! 1. 正常关停：`plugin/shutdown` → 关 stdin → 宽限期后 `kill`；
//! 2. `Child::kill_on_drop`：装载器被 drop 时兜底；
//! 3. Windows 作业对象 `KILL_ON_JOB_CLOSE`：连内核被强杀都能连坐整棵进程树。
pub mod host;
pub mod install;
pub mod loader;
pub mod manifest;

pub use host::{
    ApiVersion, Crashed, Handshake, HandshakeCommand, HandshakeTool, Host, LoadError, LoadSpec,
    Loaded, HANDSHAKE_METHOD, HANDSHAKE_TIMEOUT, LEGACY_HANDSHAKE_METHOD, METHOD_NOT_FOUND,
};
pub use install::{
    install, installed_dir, plugins_root, remove_installed, InstalledPackage, PackageError,
};
pub use loader::{
    PluginProcess, ReplyError, RequestError, SpawnError, SpawnOptions, FRAME_LIMIT_BYTES,
    SHUTDOWN_GRACE,
};
pub use manifest::{
    details_json, namespaced_tool, Manifest, ManifestCommand, ManifestTool, ValidatedManifest,
    Violation, ALLOWED_CAPABILITIES, CORE_TOOL_NAMES, DEFAULT_TIMEOUT_MS, DENIED_CAPABILITIES,
    HOST_PLUGIN_API_MAJOR, HOST_PLUGIN_API_MINOR, MANIFEST_FILE,
};
