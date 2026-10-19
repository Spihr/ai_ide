//! 密钥的内存保管。
//!
//! 架构约定（`docs/02` §6.3）：**明文密钥只存在于两处内存**——主进程的 safeStorage
//! 密文解出瞬间、以及内核处理请求的这段时间。磁盘上的自定义配置文件、日志、事件、
//! `provider.list` 响应里都不允许出现明文。
//!
//! 因此密钥在这个 crate 里的载体是 [`SecretString`]：
//!
//! - `Debug` 被打成 `sk-a…4f2c` 这样的指纹，不会被 `{:?}` 顺手打进日志；
//! - 不实现 `Serialize`，序列化时编译期报错（而不是运行期才发现漏了脱敏）；
//! - 取值必须显式调用 [`SecretString::expose`]，让「我要用明文了」在代码里可见。

use std::fmt;

/// 内存中的密钥包装。
#[derive(Clone)]
pub struct SecretString {
    value: String,
    hint: String,
}

impl SecretString {
    /// 包装一把明文密钥，并算好指纹。
    pub fn new(raw: impl Into<String>) -> Self {
        let value = raw.into();
        let hint = mask(&value);
        Self { value, hint }
    }

    /// 取出明文，用于拼装 `Authorization` 头。
    ///
    /// 命名刻意刺眼：调用点越少越好，且返回值绝不能被写进日志或事件。
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// 脱敏指纹，形如 `sk-a…4f2c`，可安全展示与落盘。
    pub fn hint(&self) -> &str {
        &self.hint
    }

    /// 是否为空密钥（空串视为「未配置」）。
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        // 尽力而为的清理：清空并归还容量，减少明文在堆上的驻留时间。
        // 这里不给「内存清零」的硬保证——Rust 的 String 可能已被移动拷贝过，
        // 真正确保清零需要 unsafe 与固定缓冲区，本阶段不做，也不在文档里吹。
        self.value.clear();
        self.value.shrink_to_fit();
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString({})", self.hint)
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 连 Display 也只给指纹：避免 `format!("{key}")` 这种手滑。
        write!(f, "{}", self.hint)
    }
}

/// 生成脱敏指纹。
///
/// - 长度 ≥ 12：保留前 4 与后 4，中间用 `…`；
/// - 更短的密钥：整体掩码，只露长度级别的信息。
fn mask(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    if chars.len() < 12 {
        return "••••".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_leaks_plaintext() {
        let secret = SecretString::new("sk-abcdefghijklmnop1234567890");
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("abcdefghijklmnop"), "Debug 泄露明文: {rendered}");
        assert!(rendered.contains('…'));
    }

    #[test]
    fn display_never_leaks_plaintext() {
        let secret = SecretString::new("sk-abcdefghijklmnop1234567890");
        let rendered = format!("{secret}");
        assert!(!rendered.contains("abcdefghijklmnop"), "Display 泄露明文: {rendered}");
    }

    #[test]
    fn hint_keeps_head_and_tail() {
        let secret = SecretString::new("sk-abcdefghijklmnop1234567890");
        let hint = secret.hint().to_string();
        assert!(hint.starts_with("sk-a"));
        assert!(hint.ends_with("7890"));
    }

    #[test]
    fn short_keys_are_fully_masked() {
        let secret = SecretString::new("short");
        assert_eq!(secret.hint(), "••••");
        assert!(!secret.hint().contains("short"));
    }

    #[test]
    fn empty_key_is_detected() {
        assert!(SecretString::new("").is_empty());
        assert!(!SecretString::new("x").is_empty());
        assert_eq!(SecretString::new("").hint(), "");
    }

    #[test]
    fn expose_returns_plaintext_for_request_headers() {
        let secret = SecretString::new("sk-test-key");
        assert_eq!(secret.expose(), "sk-test-key");
    }
}
