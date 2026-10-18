//! 协议版本与兼容性判定。
//!
//! 规则（见 `docs/03-内核接口协议规范.md` §5）：
//! - `major` 不一致 → 拒绝连接（双方不兼容）。
//! - `minor` 不一致 → 以较低版本的能力集为准，未知字段忽略。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use ts_rs::TS;

/// 当前协议版本字面量，便于写进日志与握手报文。
pub const PROTOCOL_VERSION: &str = "1.0";

/// 结构化的协议版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

impl ProtocolVersion {
    /// 当前内核实现的版本。
    pub const CURRENT: Self = Self::new(1, 0);

    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// 解析 `"1.0"` 形式的版本串；非法输入返回 [`None`]。
    pub fn parse(s: &str) -> Option<Self> {
        let (major, minor) = s.split_once('.')?;
        Some(Self {
            major: major.trim().parse().ok()?,
            minor: minor.trim().parse().ok()?,
        })
    }

    /// 主版本号一致即视为可通信。
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for ProtocolVersion {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(ParseVersionError)
    }
}

/// 版本串格式错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("协议版本格式非法，期望 \"<major>.<minor>\"，例如 \"1.0\"")]
pub struct ParseVersionError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        assert_eq!(
            ProtocolVersion::parse("1.0"),
            Some(ProtocolVersion::new(1, 0))
        );
        assert_eq!(
            ProtocolVersion::parse("2.13"),
            Some(ProtocolVersion::new(2, 13))
        );
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(ProtocolVersion::parse("1"), None);
        assert_eq!(ProtocolVersion::parse("a.b"), None);
        assert_eq!(ProtocolVersion::parse("1."), None);
        assert_eq!(ProtocolVersion::parse(""), None);
    }

    #[test]
    fn compatibility_requires_same_major() {
        let current = ProtocolVersion::CURRENT;
        assert!(current.is_compatible_with(ProtocolVersion::new(1, 99)));
        assert!(!current.is_compatible_with(ProtocolVersion::new(2, 0)));
    }

    #[test]
    fn display_matches_const() {
        assert_eq!(ProtocolVersion::CURRENT.to_string(), PROTOCOL_VERSION);
    }
}
