//! 六个核心工具，以及它们共用的参数读取器。
//!
//! 每个工具的职责边界都遵守同一套约定：
//!
//! 1. **闸门先行**：先 `guard.resolve` / `guard.resolve_for_write`，越界与敏感路径直接
//!    返回致命的 [`AgentError::Path`]，绝不当成「读不到」被吞掉。
//! 2. **失败可回灌**：找不到文件、目标不唯一、乐观锁冲突都返回非致命错误，
//!    由 `runner` 折成 `role = tool` 的消息交给模型自己纠正。

pub mod create;
pub mod edit;
pub mod grep;
pub mod list;
pub mod read;
pub mod write;

use crate::error::AgentError;
use serde_json::Value;

/// 工具参数读取器。
///
/// 解析失败统一映射成 [`AgentError::BadArgs`]——这是**可回灌**的：模型看到
/// 「参数 `path` 应为字符串」会自己改一版重发，比让整条对话失败友好得多。
pub(crate) struct Args<'a> {
    tool: &'static str,
    value: &'a Value,
}

impl<'a> Args<'a> {
    pub(crate) fn new(tool: &'static str, value: &'a Value) -> Self {
        Self { tool, value }
    }

    /// 必填非空字符串。
    pub(crate) fn str(&self, key: &str) -> Result<&'a str, AgentError> {
        let value: &'a Value = self.value;
        match value.get(key) {
            Some(Value::String(text)) if !text.trim().is_empty() => Ok(text),
            Some(Value::String(_)) => Err(self.bad(key, "不能为空")),
            Some(_) => Err(self.bad(key, "应为字符串")),
            None => Err(self.bad(key, "是必填参数")),
        }
    }

    /// 必填字符串，**允许空串**（写文件的 `content`、替换用的 `new_text` 都要它）。
    pub(crate) fn text(&self, key: &str) -> Result<&'a str, AgentError> {
        let value: &'a Value = self.value;
        match value.get(key) {
            Some(Value::String(text)) => Ok(text),
            Some(_) => Err(self.bad(key, "应为字符串")),
            None => Err(self.bad(key, "是必填参数")),
        }
    }

    /// 可选字符串；空串视为「没给」。
    pub(crate) fn opt_str(&self, key: &str) -> Result<Option<&'a str>, AgentError> {
        let value: &'a Value = self.value;
        match value.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(self.bad(key, "应为字符串")),
        }
    }

    /// 可选整数。
    pub(crate) fn opt_u32(&self, key: &str) -> Result<Option<u32>, AgentError> {
        let value: &'a Value = self.value;
        match value.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(number)) => {
                match number.as_u64().and_then(|v| u32::try_from(v).ok()) {
                    Some(number) => Ok(Some(number)),
                    None => Err(self.bad(key, "应为 0 到 4294967295 之间的整数")),
                }
            }
            Some(_) => Err(self.bad(key, "应为整数")),
        }
    }

    /// 可选布尔。
    pub(crate) fn opt_bool(&self, key: &str) -> Result<Option<bool>, AgentError> {
        let value: &'a Value = self.value;
        match value.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(flag)) => Ok(Some(*flag)),
            Some(_) => Err(self.bad(key, "应为布尔值")),
        }
    }

    /// 可选字符串数组；缺省为空数组。
    pub(crate) fn opt_str_list(&self, key: &str) -> Result<Vec<String>, AgentError> {
        let value: &'a Value = self.value;
        match value.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::String(text) => out.push(text.clone()),
                        _ => return Err(self.bad(key, "应为字符串数组")),
                    }
                }
                Ok(out)
            }
            Some(_) => Err(self.bad(key, "应为字符串数组")),
        }
    }

    fn bad(&self, key: &str, reason: &str) -> AgentError {
        AgentError::BadArgs {
            tool: self.tool.to_string(),
            reason: format!("参数 `{key}` {reason}"),
        }
    }
}
