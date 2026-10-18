//! 方法名常量表。
//!
//! 命名规范：`<域>.<动作>`，域与动作均小写下划线（见 `docs/03-内核接口协议规范.md` §1.2）。
//! 事件（Core → Main 的单向通知）与请求同名空间，以 `methods::event` 分区区分。

/// 内核生命周期。
pub mod core {
    pub const HELLO: &str = "core.hello";
    pub const PING: &str = "core.ping";
    pub const STATS: &str = "core.stats";
    pub const SHUTDOWN: &str = "core.shutdown";
}

/// 取消机制。
pub mod cancel {
    /// 取消某个在途请求（其 `params.id` 指向被取消请求的 id）。
    pub const REQUEST: &str = "$/cancelRequest";
}

/// 工作区与文件系统（M1）。
pub mod workspace {
    pub const OPEN: &str = "workspace.open";
    pub const CLOSE: &str = "workspace.close";
    pub const TREE: &str = "workspace.tree";
    pub const READ_FILE: &str = "workspace.read_file";
    pub const WRITE_FILE: &str = "workspace.write_file";
    pub const CREATE: &str = "workspace.create";
    pub const RENAME: &str = "workspace.rename";
    pub const DELETE: &str = "workspace.delete";
    pub const REVEAL: &str = "workspace.reveal";
}

/// 检索（M1：文件名模糊 + 内容检索；符号检索属 M3）。
pub mod search {
    pub const FILES: &str = "search.files";
    pub const CONTENT: &str = "search.content";
}

/// Core → Main 的通知（事件）。
pub mod event {
    /// 内核日志转发，便于 Main 侧统一落盘。
    pub const LOG: &str = "core.log";
    /// 工作区内的文件被外部程序改动。
    pub const WORKSPACE_CHANGED: &str = "workspace.changed";
    /// 后台全量索引完成，附带最终规模统计。
    pub const WORKSPACE_INDEXED: &str = "workspace.indexed";
}

/// 内核已实现的方法全集，用于单测与能力声明。
///
/// 注意：事件（通知）不计入能力清单——能力清单描述的是「可被请求的方法」。
pub const IMPLEMENTED: &[&str] = &[
    core::HELLO,
    core::PING,
    core::STATS,
    core::SHUTDOWN,
    cancel::REQUEST,
    workspace::OPEN,
    workspace::CLOSE,
    workspace::TREE,
    workspace::READ_FILE,
    workspace::WRITE_FILE,
    workspace::CREATE,
    workspace::RENAME,
    workspace::DELETE,
    workspace::REVEAL,
    search::FILES,
    search::CONTENT,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implemented_list_covers_core_and_m1_domains() {
        assert_eq!(IMPLEMENTED.len(), 16);
        assert!(IMPLEMENTED.contains(&"core.ping"));
        assert!(IMPLEMENTED.contains(&"$/cancelRequest"));
        assert!(IMPLEMENTED.contains(&"workspace.read_file"));
        assert!(IMPLEMENTED.contains(&"search.content"));
    }

    #[test]
    fn every_implemented_name_is_domain_dot_action() {
        for name in IMPLEMENTED {
            if *name == cancel::REQUEST {
                continue;
            }
            let (domain, action) = name.split_once('.').expect("方法名必须形如 <域>.<动作>");
            assert!(!domain.is_empty(), "域不能为空: {name}");
            assert!(!action.is_empty(), "动作不能为空: {name}");
            for ch in name.chars() {
                assert!(
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_',
                    "方法名含非法字符 {ch}: {name}"
                );
            }
        }
    }

    #[test]
    fn workspace_module_matches_protocol_doc() {
        assert_eq!(workspace::OPEN, "workspace.open");
        assert_eq!(workspace::READ_FILE, "workspace.read_file");
        assert_eq!(workspace::WRITE_FILE, "workspace.write_file");
        assert_eq!(search::FILES, "search.files");
        assert_eq!(event::WORKSPACE_CHANGED, "workspace.changed");
    }
}
