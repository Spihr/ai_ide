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

/// Provider 与模型管理（M2，已在 `crates/core` 接线）。
///
/// 密钥本身不在这层：`provider.upsert` 的 `api_key` 只进内存保管（见 docs/02 §6.3），
/// `provider.list` 只回 `has_key` / `key_hint` 这类脱敏信息。
pub mod provider {
    pub const UPSERT: &str = "provider.upsert";
    pub const LIST: &str = "provider.list";
    pub const DELETE: &str = "provider.delete";
    pub const TEST: &str = "provider.test";
}

/// 模型目录（M2，已在 `crates/core` 接线）。模型挂在 Provider 下，把「厂商」与「具体模型」解耦。
pub mod model {
    pub const UPSERT: &str = "model.upsert";
    pub const LIST: &str = "model.list";
    pub const DELETE: &str = "model.delete";
}

/// 模型接入（M2：流式对话、取消、模型枚举、token 计数）。
pub mod llm {
    pub const CHAT_STREAM: &str = "llm.chat_stream";
    pub const CANCEL: &str = "llm.cancel";
    pub const LIST_MODELS: &str = "llm.list_models";
    pub const COUNT_TOKENS: &str = "llm.count_tokens";
}

/// Core → Main 的通知（事件）。
pub mod event {
    /// 内核日志转发，便于 Main 侧统一落盘。
    pub const LOG: &str = "core.log";
    /// 工作区内的文件被外部程序改动。
    pub const WORKSPACE_CHANGED: &str = "workspace.changed";
    /// 后台全量索引完成，附带最终规模统计。
    pub const WORKSPACE_INDEXED: &str = "workspace.indexed";
    /// 流式对话开始（M2）。每次 `llm.chat_stream` 恰好一条。
    pub const LLM_STARTED: &str = "llm.started";
    /// 正文增量（M2）。
    pub const LLM_DELTA: &str = "llm.delta";
    /// 思维链增量（M2，仅推理模型有）。
    pub const LLM_REASONING: &str = "llm.reasoning";
    /// 工具调用增量（M2，参数按片段拼接）。
    pub const LLM_TOOL_CALL: &str = "llm.tool_call";
    /// 用量统计（M2）。
    pub const LLM_USAGE: &str = "llm.usage";
    /// 流正常结束（M2）。与 [`LLM_ERROR`] 互斥。
    pub const LLM_FINISHED: &str = "llm.finished";
    /// 流异常终止（M2）。与 [`LLM_FINISHED`] 互斥。
    pub const LLM_ERROR: &str = "llm.error";
    /// Provider 目录发生变化（M2），供设置界面刷新。
    pub const PROVIDER_CHANGED: &str = "provider.changed";
}

/// 内核**真正接线并可路由**的方法全集，用于单测与能力声明。
///
/// 注意：事件（通知）不计入能力清单——能力清单描述的是「可被请求的方法」。
///
/// 这里的「已实现」有严格含义：`crates/core` 的 `dispatch.rs` 里有对应分支，
/// 调用不会返回 `-32601`。协议里**定义了但还没接线**的方法见
/// [`DECLARED_NOT_IMPLEMENTED`]。
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
    provider::UPSERT,
    provider::LIST,
    provider::DELETE,
    provider::TEST,
    model::UPSERT,
    model::LIST,
    model::DELETE,
    llm::CHAT_STREAM,
    llm::CANCEL,
    llm::LIST_MODELS,
    llm::COUNT_TOKENS,
];

/// 协议面**已定义、但内核尚未接线**的方法。
///
/// **M2 收尾后本表为空**：`provider.*` / `model.*` / `llm.*` 都已接进
/// `crates/core` 的 `dispatch.rs`。空表不是「这个机制没用」——它是给后续里程碑
/// （M3 会话、M4 Agent、M5 终端…）留的位置：协议先行定义、实现后补，谁没接线
/// 谁就待在这里。
///
/// 它们**不能**出现在 [`IMPLEMENTED`] 里：`IMPLEMENTED` 会经 `core.hello`
/// 回报给 Main 作为能力清单，一旦虚报，Main 会照单调用、然后拿到一片 `-32601`。
/// 接线时把对应常量从本表移入 [`IMPLEMENTED`]，两张表始终互斥。
pub const DECLARED_NOT_IMPLEMENTED: &[&str] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implemented_list_covers_core_m1_m2_domains() {
        assert_eq!(IMPLEMENTED.len(), 27);
        assert!(IMPLEMENTED.contains(&"core.ping"));
        assert!(IMPLEMENTED.contains(&"$/cancelRequest"));
        assert!(IMPLEMENTED.contains(&"workspace.read_file"));
        assert!(IMPLEMENTED.contains(&"search.content"));
        assert!(IMPLEMENTED.contains(&provider::UPSERT));
        assert!(IMPLEMENTED.contains(&model::DELETE));
        // M2 收尾：`llm.*` 四个方法进清单，能力声明与 dispatch 分支一致。
        assert!(IMPLEMENTED.contains(&llm::CHAT_STREAM));
        assert!(IMPLEMENTED.contains(&llm::CANCEL));
        assert!(IMPLEMENTED.contains(&llm::LIST_MODELS));
        assert!(IMPLEMENTED.contains(&llm::COUNT_TOKENS));
    }

    #[test]
    fn declared_table_is_empty_after_m2_wiring() {
        // M2 把 `llm.*` 接进 dispatch 后，没有「定义了但没接线」的方法了。
        // 一旦有人新增协议方法却忘了接线，这里会先响——比 Main 侧收到
        // 一片 `-32601` 才发现要好得多。
        assert!(
            DECLARED_NOT_IMPLEMENTED.is_empty(),
            "M2 收尾后待接线方法表应为空，实际：{DECLARED_NOT_IMPLEMENTED:?}"
        );
        for name in [
            llm::CHAT_STREAM,
            llm::CANCEL,
            llm::LIST_MODELS,
            llm::COUNT_TOKENS,
        ] {
            assert!(IMPLEMENTED.contains(&name), "{name} 应已接线");
        }
    }

    #[test]
    fn capability_tables_are_disjoint_and_cover_every_request_method() {
        let mut all: Vec<&str> = IMPLEMENTED.to_vec();
        all.extend_from_slice(DECLARED_NOT_IMPLEMENTED);
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total, "两张表不得有重叠");
        assert_eq!(total, 27, "应恰好覆盖协议里定义的全部 27 个请求方法");
    }

    #[test]
    fn events_are_not_part_of_the_capability_surface() {
        // 事件是单向通知，不能被请求，因此两张方法表里都不该出现。
        for event in [
            event::LOG,
            event::WORKSPACE_CHANGED,
            event::LLM_STARTED,
            event::LLM_DELTA,
            event::LLM_REASONING,
            event::LLM_TOOL_CALL,
            event::LLM_USAGE,
            event::LLM_FINISHED,
            event::LLM_ERROR,
            event::PROVIDER_CHANGED,
        ] {
            assert!(!IMPLEMENTED.contains(&event), "事件不该进能力清单: {event}");
            assert!(
                !DECLARED_NOT_IMPLEMENTED.contains(&event),
                "事件不该进待接线方法表: {event}"
            );
        }
    }

    #[test]
    fn llm_domain_matches_protocol_doc() {
        assert_eq!(provider::UPSERT, "provider.upsert");
        assert_eq!(model::LIST, "model.list");
        assert_eq!(llm::CHAT_STREAM, "llm.chat_stream");
        assert_eq!(llm::COUNT_TOKENS, "llm.count_tokens");
        assert_eq!(event::LLM_DELTA, "llm.delta");
        assert_eq!(event::PROVIDER_CHANGED, "provider.changed");
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
