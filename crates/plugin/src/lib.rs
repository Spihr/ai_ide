//! 插件系统 P1 内核侧：清单校验 + 安装目录/包复制。
//!
//! 这一片刻意不含任何进程管理：没有 `tokio::process`、没有握手、没有超时监控。
//! `plugin.load` 之后的事情属于 P2，边界写在这里是为了让「安装」这个动作的
//! 副作用只剩磁盘——验收时才能用「装完没有子进程」这种断言把它钉住。
//!
//! 对外只有两件事：
//!
//! - [`manifest`]：清单结构、能力白名单、三段校验（形状 → 语义 → 交叉）；
//! - [`install`]：读包 → 校验 → 复制到 `<data_dir>/plugins/<id>`，以及反向删除。

pub mod install;
pub mod manifest;

pub use install::{
    install, installed_dir, plugins_root, remove_installed, InstalledPackage, PackageError,
};
pub use manifest::{
    details_json, namespaced_tool, Manifest, ManifestCommand, ManifestTool, ValidatedManifest,
    Violation, ALLOWED_CAPABILITIES, CORE_TOOL_NAMES, DEFAULT_TIMEOUT_MS, DENIED_CAPABILITIES,
    HOST_PLUGIN_API_MAJOR, HOST_PLUGIN_API_MINOR, MANIFEST_FILE,
};
