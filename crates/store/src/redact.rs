//! 敏感字段脱敏。
//!
//! 审计日志的价值是「事后能查」，代价是它会把标题、导出路径这类用户数据抄一份进
//! 库。只要有人把密钥粘进标题，审计表就成了新的泄漏源——所以写入前统一过一遍，
//! 读出来再兜一遍（老版本写进来的行、被手改过的库都算在内）。
//!
//! 两层策略：
//! - **按键名**：[`is_sensitive_key`] 判定 `api_key` / `token` / `password` 这类键，
//!   值直接打码。先把键名归一化（去掉非字母数字并转小写），`apiKey` 与 `api_key`
//!   才走同一条规则。
//! - **按取值**：[`redact_text`] 处理「键名无害、值里却藏着密钥」的文本，例如
//!   `标题：sk-abcdef123456`。只认前缀特征（`sk-` / `ghp_` / `Bearer ` 等），
//!   不做熵估计——误伤一段正常的英文句子比漏掉一个密钥更让人不敢用审计。

use serde_json::Value;

/// 打码后的占位符。
pub const MASK: &str = "***";

/// 键名归一化后命中这些子串就认为值敏感。
const SENSITIVE_KEY_NEEDLES: &[&str] = &[
    "apikey",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "authorization",
    "auth",
    "credential",
    "privatekey",
    "accesskey",
    "secretkey",
    "sessionkey",
    "cookie",
    "bearer",
    "signature",
];

/// 明确**不**敏感的白名单（归一化后的全名）。
///
/// 这些键长得像敏感键，其实是计数 / 预算 / 模式。打码只会把审计变成一排 `***`，
/// 反而没法查。
const SAFE_KEY_EXACT: &[&str] = &[
    "maxtokens",
    "prompttokens",
    "completiontokens",
    "totaltokens",
    "tokenbudget",
    "tokenlimit",
    "tokencount",
    "authmode",
    "cookiecount",
];

/// 会被当作密钥开头的特征前缀（大小写敏感，`AKIA` 只出现在大写下）。
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "AIza",
    "Bearer ",
    "bearer ",
];

/// 前缀之后至少要有这么多字符才算「像密钥」。
///
/// 门槛设在 8：`sk-1` 这种是片段不是密钥，误打码只会让文本读不通。
const MIN_SECRET_BODY: usize = 8;

/// 键名归一化：只保留 ASCII 字母数字并转小写。
fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// 键名是否代表敏感信息。
pub fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    if normalized.is_empty() || SAFE_KEY_EXACT.contains(&normalized.as_str()) {
        return false;
    }
    SENSITIVE_KEY_NEEDLES
        .iter()
        .any(|needle| normalized.contains(needle))
}

/// 把一段密钥取值打码：短值整段替换，长值保留首尾各 4 位便于人工比对。
///
/// 保留位不是为了「方便调试」而是为了让运维能回答「是不是同一把 key」，
/// 4 位在 32 位以上的密钥上不足以还原任何可用信息。
pub fn mask_secret(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 12 {
        // 短值不留任何位：`sk-1234` 这类长度本身就是大半信息。
        return MASK.to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}{MASK}{tail}")
}

/// 判断 `chars[index]` 是否处在一个「词的开头」，避免把 `task-manager` 里的
/// `sk-` 误当密钥前缀。
fn is_word_start(chars: &[char], index: usize) -> bool {
    match index.checked_sub(1).map(|prev| chars[prev]) {
        None => true,
        Some(prev) => !(prev.is_alphanumeric() || prev == '-' || prev == '_'),
    }
}

/// 密钥正文允许的字符：字母数字与 `-` `_`。刻意不含 `.`，
/// 免得把句号后的整句英文一起吞掉。
fn is_secret_body_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '-' || ch == '_'
}

/// 对自由文本做模式脱敏。
pub fn redact_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if is_word_start(&chars, index) {
            if let Some(prefix) = SECRET_PREFIXES
                .iter()
                .find(|prefix| starts_with_at(&chars, index, prefix))
            {
                let body_start = index + prefix.chars().count();
                let mut body_end = body_start;
                while body_end < chars.len() && is_secret_body_char(chars[body_end]) {
                    body_end += 1;
                }
                if body_end - body_start >= MIN_SECRET_BODY {
                    let body: String = chars[body_start..body_end].iter().collect();
                    out.push_str(prefix);
                    out.push_str(&mask_secret(&body));
                    index = body_end;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn starts_with_at(chars: &[char], index: usize, prefix: &str) -> bool {
    let mut cursor = index;
    for expected in prefix.chars() {
        match chars.get(cursor) {
            Some(actual) if *actual == expected => cursor += 1,
            _ => return false,
        }
    }
    true
}

/// 对任意 JSON 值递归脱敏：敏感键整段打码，其余字符串按模式处理。
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, entry) in map {
                let redacted = if is_sensitive_key(key) {
                    match entry {
                        Value::Null => Value::Null,
                        // 字符串也整段打码，不保留首尾：键名已经说明「这是密钥」，
                        // 留下几位只方便了撞库的人（`sk-a***mnop` 足以缩小搜索空间）。
                        // 保留边角的 [`mask_secret`] 只用于自由文本——那里没得选。
                        Value::String(text) if text.is_empty() => Value::String(String::new()),
                        // 数组 / 对象 / 数字一律压成占位符：敏感键下面再拆结构，
                        // 泄漏的往往是「有几个元素」这种同样不该外露的信息。
                        _ => Value::String(MASK.to_string()),
                    }
                } else {
                    redact_value(entry)
                };
                out.insert(key.clone(), redacted);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::String(text) => Value::String(redact_text(text)),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_key_names_are_recognized_in_any_spelling() {
        for key in [
            "api_key",
            "apiKey",
            "API-KEY",
            "access_token",
            "refreshToken",
            "password",
            "client_secret",
            "Authorization",
            "private_key",
            "session_cookie",
        ] {
            assert!(is_sensitive_key(key), "{key} 应判定为敏感");
        }

        for key in ["title", "path", "max_tokens", "token_budget", "auth_mode"] {
            assert!(!is_sensitive_key(key), "{key} 不应判定为敏感");
        }
    }

    #[test]
    fn short_secrets_are_masked_whole() {
        assert_eq!(mask_secret("sk-1234"), MASK);
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("   "), "");
    }

    #[test]
    fn long_secrets_keep_only_edges() {
        let masked = mask_secret("sk-abcdefghijklmnop");
        assert_eq!(masked, "sk-a***mnop");
        assert!(!masked.contains("efgh"));
    }

    #[test]
    fn text_redaction_catches_prefixed_tokens() {
        let text = "用户把 sk-abcdefgh12345678 粘进了标题";
        let redacted = redact_text(text);
        assert!(redacted.contains("sk-abcd***5678"), "实际：{redacted}");
        assert!(!redacted.contains("abcdefgh12345678"));
    }

    #[test]
    fn text_redaction_ignores_lookalikes_and_short_fragments() {
        // 词中出现的 `sk-`（task-manager）不是前缀；`sk-1` 长度不够。
        assert_eq!(redact_text("task-manager 与 sk-1"), "task-manager 与 sk-1");
        assert_eq!(redact_text("普通文本，没有密钥"), "普通文本，没有密钥");
    }

    #[test]
    fn value_redaction_walks_objects_and_arrays() {
        let value = serde_json::json!({
            "title": "带 sk-abcdefgh1234 的标题",
            "token": "short",
            "nested": { "apiKey": ["a", "b"], "note": "ok" },
            "list": [{ "password": "hunter2hunter2" }],
        });
        let redacted = redact_value(&value);
        assert_eq!(redacted["token"], serde_json::json!(MASK));
        assert_eq!(redacted["nested"]["apiKey"], serde_json::json!(MASK));
        assert_eq!(redacted["nested"]["note"], serde_json::json!("ok"));
        assert_eq!(redacted["list"][0]["password"], serde_json::json!(MASK));
        let title = redacted["title"].as_str().unwrap();
        assert!(!title.contains("abcdefgh1234"), "实际：{title}");
    }
}
