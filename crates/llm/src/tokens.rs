//! token 计数。
//!
//! 两条口径，用同一个 `exact` 标志对外区分：
//!
//! - **精确**（`exact: true`）：模型族已知时接 `tiktoken-rs` 的真实 BPE 分词器，
//!   按 [`encoding_for_model`] 选 `o200k_base` / `cl100k_base` 词表（词表随二进制内嵌），
//!   文本部分逐字对齐官方 tokenizer；
//! - **估算**（`exact: false`）：模型族未识别（非 OpenAI 命名的自建 / 第三方模型名，
//!   以及 M2 老模型族）时回落本地估算器，口径见下。
//!
//! 估算口径参考主流 BPE 的实测经验值：
//!
//! - CJK 字符 ≈ 1 token / 字；
//! - 其它非 ASCII 字符 ≈ 1 token / 字（保守）；
//! - ASCII 可见字符 ≈ 1 token / 4 字符；
//! - 每条消息 ≈ 4 token 结构开销，工具声明按其 JSON 文本再估一次。
//!
//! 误差量级在 ±20% 以内，足够驱动 UI 的上下文占用条；真正精确的计数
//! 由上游返回的 `usage` 校正（见 `llm.usage` 事件）。

use std::sync::OnceLock;

use ai_ide_protocol::dto::{ChatMessage, ToolSpec};
use tiktoken_rs::{cl100k_base, o200k_base, CoreBPE};

/// 每条消息固定开销（role、分隔符等）。
const PER_MESSAGE_OVERHEAD: u64 = 4;

/// 估算一组消息 + 工具声明的 token 数。
pub fn estimate_tokens(messages: &[ChatMessage], tools: &[ToolSpec]) -> u64 {
    let mut total: u64 = 0;
    for message in messages {
        total += PER_MESSAGE_OVERHEAD;
        total += estimate_text(&message.content);
        for call in &message.tool_calls {
            total += estimate_text(&call.name);
            total += estimate_text(&call.arguments);
        }
    }
    for tool in tools {
        total += estimate_text(&tool.name);
        if let Some(description) = &tool.description {
            total += estimate_text(description);
        }
        total += estimate_text(&tool.parameters.to_string());
    }
    total
}

/// 估算一段纯文本的 token 数。
pub fn estimate_text(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let mut wide: u64 = 0;
    let mut ascii: u64 = 0;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    // ASCII 部分按 4 字符 1 token 折算，不足 1 个 token 的零头也计 1。
    let ascii_tokens = ascii.div_ceil(4);
    wide + ascii_tokens
}

/// 面向 UI 的估算口径说明（写进 `llm.count_tokens` 的 `note`，`exact: false`）。
pub const ESTIMATE_NOTE: &str =
    "未识别模型族，回落估算：「CJK 1 字≈1 token、ASCII 4 字符≈1 token、每消息 +4」，误差约 ±20%";

/// 精确口径说明：`o200k_base`（写进 `llm.count_tokens` 的 `note`，`exact: true`）。
pub const EXACT_NOTE_O200K: &str =
    "按 tiktoken o200k_base 词表精确分词（文本部分与官方 tokenizer 一致），结构开销按每消息 +4 计";

/// 精确口径说明：`cl100k_base`（写进 `llm.count_tokens` 的 `note`，`exact: true`）。
pub const EXACT_NOTE_CL100K: &str =
    "按 tiktoken cl100k_base 词表精确分词（文本部分与官方 tokenizer 一致），结构开销按每消息 +4 计";

// ---------------------------------------------------------------------------
// 精确口径：真实分词器
// ---------------------------------------------------------------------------

/// 已接入的真实分词编码族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenEncoding {
    /// GPT-4o / GPT-4.1 / GPT-4.5 / GPT-5 / `o*` 系列用的 200k 词表。
    O200kBase,
    /// GPT-4 / GPT-3.5-turbo / text-embedding-3 系列用的 100k 词表。
    Cl100kBase,
}

impl TokenEncoding {
    /// 编码名（写进 `note`，方便对账）。
    pub fn name(self) -> &'static str {
        match self {
            Self::O200kBase => "o200k_base",
            Self::Cl100kBase => "cl100k_base",
        }
    }

    /// 取词表。进程内只解析一次（词表很大，别每次敲键盘都重建）。
    ///
    /// 解析失败时返回 `None`（例如内嵌词表与运行时环境不匹配），由调用方回落到估算——
    /// 计数是 UI 的辅助信息，不该因为分词器起不来就把整条链路弄崩。
    fn bpe(self) -> Option<&'static CoreBPE> {
        static O200K: OnceLock<Option<CoreBPE>> = OnceLock::new();
        static CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();
        let slot = match self {
            Self::O200kBase => &O200K,
            Self::Cl100kBase => &CL100K,
        };
        slot.get_or_init(|| {
            if self == Self::O200kBase {
                o200k_base().ok()
            } else {
                cl100k_base().ok()
            }
        })
        .as_ref()
    }
}

/// 按上游模型名判断编码族；未识别返回 `None`。
///
/// 映射取自 OpenAI 官方 `tiktoken` 的 `encoding_for_model` 前缀表，只保留本项目接入的两族；
/// 其余（`r50k_base` / `p50k_base` 的老 GPT-3 系、Claude / DeepSeek / Qwen 等非 OpenAI 命名）
/// 一律返回 `None`，由调用方回落估算——宁可标「估算」，也不拿别的词表冒充。
pub fn encoding_for_model(model_id: &str) -> Option<TokenEncoding> {
    let model = strip_finetune_prefix(model_id);

    // o200k_base：GPT-4o / GPT-4.1 / GPT-4.5 / GPT-5 / o1·o3·o4。
    if model == "gpt-4o"
        || model.starts_with("gpt-4o-")
        || model == "chatgpt-4o-latest"
        || model.starts_with("chatgpt-4o-")
        || model == "gpt-4.1"
        || model.starts_with("gpt-4.1-")
        || model.starts_with("gpt-4.5")
        || model.starts_with("gpt-5")
        || model == "o1"
        || model.starts_with("o1-")
        || model == "o3"
        || model.starts_with("o3-")
        || model.starts_with("o4-")
    {
        return Some(TokenEncoding::O200kBase);
    }

    // cl100k_base：GPT-4 / GPT-3.5-turbo / text-embedding。
    if model == "gpt-4"
        || model.starts_with("gpt-4-")
        || model == "gpt-3.5-turbo"
        || model.starts_with("gpt-3.5-turbo-")
        || model == "gpt-3.5"
        || model.starts_with("gpt-3.5-")
        || model == "gpt-35-turbo"
        || model.starts_with("gpt-35-turbo-")
        || model.starts_with("text-embedding-")
    {
        return Some(TokenEncoding::Cl100kBase);
    }

    None
}

/// 微调模型名形如 `ft:gpt-4o-mini-2024-07-18:org:name:hash`，取冒号前的基础模型名。
fn strip_finetune_prefix(model_id: &str) -> &str {
    match model_id.strip_prefix("ft:") {
        Some(rest) => rest.split(':').next().unwrap_or(rest),
        None => model_id,
    }
}

/// 一次计数的口径：认识模型族就精确，不认识就估算。
///
/// 两条口径的结构开销（每条消息 +4）完全一致，只有文本部分是「分词」还是「折算」的区别，
/// 因此引用预算裁剪（走 [`estimate_text`]）的行为不受精确口径影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenCounter {
    /// 已接真实分词器。
    Exact(TokenEncoding),
    /// 未识别模型族，回落估算。
    Estimated,
}

impl TokenCounter {
    /// 按模型名挑口径。
    pub fn for_model(model_id: &str) -> Self {
        match encoding_for_model(model_id).filter(|encoding| encoding.bpe().is_some()) {
            Some(encoding) => Self::Exact(encoding),
            None => Self::Estimated,
        }
    }

    /// 是否精确口径（对应 `llm.count_tokens` 的 `exact`）。
    pub fn is_exact(self) -> bool {
        matches!(self, Self::Exact(_))
    }

    /// 精确口径下使用的编码名；估算口径为 `None`。
    pub fn encoding_name(self) -> Option<&'static str> {
        match self {
            Self::Exact(encoding) => Some(encoding.name()),
            Self::Estimated => None,
        }
    }

    /// 面向 UI 的口径说明（写进 `llm.count_tokens` 的 `note`）。
    pub fn note(self) -> &'static str {
        match self {
            Self::Exact(TokenEncoding::O200kBase) => EXACT_NOTE_O200K,
            Self::Exact(TokenEncoding::Cl100kBase) => EXACT_NOTE_CL100K,
            Self::Estimated => ESTIMATE_NOTE,
        }
    }

    /// 计一段纯文本。
    pub fn count_text(self, text: &str) -> u64 {
        if text.is_empty() {
            return 0;
        }
        match self {
            Self::Exact(encoding) => match encoding.bpe() {
                Some(bpe) => bpe.encode_ordinary(text).len() as u64,
                None => estimate_text(text),
            },
            Self::Estimated => estimate_text(text),
        }
    }

    /// 计一组消息（含每条消息的固定结构开销）。
    pub fn count_messages(self, messages: &[ChatMessage]) -> u64 {
        match self {
            // 估算口径直接复用既有估算器，保证回落路径逐值不变。
            Self::Estimated => estimate_tokens(messages, &[]),
            Self::Exact(_) => {
                let mut total: u64 = 0;
                for message in messages {
                    total += PER_MESSAGE_OVERHEAD;
                    total += self.count_text(&message.content);
                    for call in &message.tool_calls {
                        total += self.count_text(&call.name);
                        total += self.count_text(&call.arguments);
                    }
                }
                total
            }
        }
    }

    /// 计一组工具声明。
    pub fn count_tools(self, tools: &[ToolSpec]) -> u64 {
        match self {
            Self::Estimated => estimate_tokens(&[], tools),
            Self::Exact(_) => {
                let mut total: u64 = 0;
                for tool in tools {
                    total += self.count_text(&tool.name);
                    if let Some(description) = &tool.description {
                        total += self.count_text(description);
                    }
                    total += self.count_text(&tool.parameters.to_string());
                }
                total
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_protocol::dto::{ChatRole, ToolChoice};

    fn msg(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_string(),
            attachments: vec![],
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    #[test]
    fn empty_input_costs_nothing() {
        assert_eq!(estimate_tokens(&[], &[]), 0);
        assert_eq!(estimate_text(""), 0);
    }

    #[test]
    fn ascii_is_about_four_chars_per_token() {
        assert_eq!(estimate_text("abcd"), 1);
        assert_eq!(estimate_text("abcde"), 2);
        assert_eq!(estimate_text("abcdefgh"), 2);
    }

    #[test]
    fn cjk_is_about_one_token_per_char() {
        assert_eq!(estimate_text("你好世界"), 4);
    }

    #[test]
    fn mixed_text_sums_both_buckets() {
        // 4 个 ASCII（1 token）+ 2 个汉字（2 token）
        assert_eq!(estimate_text("hi你好"), 1 + 2);
    }

    #[test]
    fn messages_add_per_message_overhead() {
        let messages = vec![msg(ChatRole::User, "abcd")];
        assert_eq!(estimate_tokens(&messages, &[]), PER_MESSAGE_OVERHEAD + 1);
    }

    #[test]
    fn tool_specs_count_name_description_and_schema() {
        let tool = ToolSpec {
            name: "read_file".to_string(),
            description: Some("读取文件".to_string()),
            parameters: serde_json::json!({ "type": "object" }),
        };
        let with_tool = estimate_tokens(&[], std::slice::from_ref(&tool));
        assert!(with_tool > 0, "工具声明必须计入");
        // tool_choice 不参与估算，这里只是顺带确认枚举可用（避免未使用告警）。
        let _ = ToolChoice::Auto;
    }

    // -----------------------------------------------------------------------
    // 精确口径（M3）
    // -----------------------------------------------------------------------

    /// 固定向量：英文代码片段 + 中文文本 + 中英混排，覆盖两族与估算口径的对账。
    const VECTORS: &[&str] = &[
        "fn main() {\n    let xs = vec![1, 2, 3];\n    println!(\"{:?}\", xs);\n}\n",
        "use std::collections::HashMap;\n\npub fn count(words: &[&str]) -> HashMap<String, usize> {\n    let mut hits = HashMap::new();\n    for word in words {\n        *hits.entry((*word).to_string()).or_insert(0) += 1;\n    }\n    hits\n}\n",
        "你好，世界！这是一个用于校验 token 计数精度的中文句子，含标点、数字 123 与英文单词 tokenizer。",
        "M3 上下文引用：把选中的代码片段（selection）注入第一条 system 消息；超出预算时按行裁剪。",
        "def add(a, b):\n    return a + b  # 求和\n",
    ];

    #[test]
    fn 模型族映射_覆盖_两族_与_未知() {
        for model in [
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4o-2024-08-06",
            "chatgpt-4o-latest",
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4.5-preview",
            "gpt-5",
            "gpt-5-mini",
            "o1",
            "o1-mini",
            "o3-mini",
            "o4-mini",
            "ft:gpt-4o-mini-2024-07-18:org:name:hash",
        ] {
            assert_eq!(
                encoding_for_model(model),
                Some(TokenEncoding::O200kBase),
                "{model} 应落到 o200k_base"
            );
        }

        for model in [
            "gpt-4",
            "gpt-4-32k",
            "gpt-4-turbo",
            "gpt-4-turbo-2024-04-09",
            "gpt-3.5-turbo",
            "gpt-3.5-turbo-0125",
            "gpt-35-turbo",
            "text-embedding-3-small",
            "text-embedding-3-large",
            "text-embedding-ada-002",
        ] {
            assert_eq!(
                encoding_for_model(model),
                Some(TokenEncoding::Cl100kBase),
                "{model} 应落到 cl100k_base"
            );
        }

        // 未接的词表 / 非 OpenAI 命名一律不猜：宁可标估算，也不拿别的词表冒充。
        for model in [
            "deepseek-chat",
            "claude-3-5-sonnet",
            "qwen2.5-coder",
            "glm-4",
            "gpt-oss-20b",
            "davinci",
            "text-davinci-003",
            "",
        ] {
            assert_eq!(encoding_for_model(model), None, "{model} 应回落估算");
            assert!(!TokenCounter::for_model(model).is_exact(), "{model}");
            assert_eq!(TokenCounter::for_model(model).note(), ESTIMATE_NOTE);
        }
    }

    #[test]
    fn 精确计数_与官方分词器逐字对齐_误差小于百分之三() {
        for (model, encoding) in [
            ("gpt-4o", TokenEncoding::O200kBase),
            ("gpt-4.1-mini", TokenEncoding::O200kBase),
            ("o3-mini", TokenEncoding::O200kBase),
            ("gpt-4-turbo", TokenEncoding::Cl100kBase),
            ("gpt-3.5-turbo", TokenEncoding::Cl100kBase),
            ("text-embedding-3-small", TokenEncoding::Cl100kBase),
        ] {
            let counter = TokenCounter::for_model(model);
            assert!(counter.is_exact(), "{model} 应走精确口径");
            assert_eq!(counter.encoding_name(), Some(encoding.name()), "{model}");
            assert!(
                counter.note().contains(encoding.name()),
                "{model} 的 note 要点明词表"
            );

            // 参考值直接来自 tiktoken-rs 的官方入口：这就是「官方 tokenizer」本身。
            let bpe = encoding.bpe().expect("词表随二进制内嵌，应可用");
            for vector in VECTORS {
                let reference = bpe.encode_ordinary(vector).len() as u64;
                let ours = counter.count_text(vector);
                assert!(reference > 0, "向量不该是空串：{vector:?}");
                let error = (ours as f64 - reference as f64).abs() / reference as f64;
                assert!(
                    error < 0.03,
                    "「{model}」对向量 {vector:?} 的相对误差 {error:.4} 超出 3%：ours={ours} ref={reference}"
                );
            }
        }
    }

    #[test]
    fn 未知模型族_回落估算_与既有估算器逐值一致() {
        let counter = TokenCounter::for_model("deepseek-chat");
        assert!(!counter.is_exact());
        assert_eq!(counter.encoding_name(), None);

        let messages = vec![msg(ChatRole::User, "hi你好")];
        assert_eq!(
            counter.count_messages(&messages),
            estimate_tokens(&messages, &[]),
            "回落路径必须与 M2 的估算器逐值一致，不能悄悄换口径"
        );
        assert_eq!(
            counter.count_text("export const draft = 1;\n"),
            estimate_text("export const draft = 1;\n")
        );
        assert_eq!(
            counter.count_tools(&[]),
            estimate_tokens(&[], &[]),
            "空列表两个口径都是 0"
        );
    }

    #[test]
    fn 精确口径_结构开销与估算一致_只有文本换成分词() {
        let counter = TokenCounter::for_model("gpt-4o");
        let messages = vec![msg(ChatRole::User, "abcd")];
        assert_eq!(
            counter.count_messages(&messages),
            PER_MESSAGE_OVERHEAD + counter.count_text("abcd"),
            "结构开销沿用「每消息 +4」，不随口径变化"
        );

        let tool = ToolSpec {
            name: "read_file".to_string(),
            description: Some("读取文件".to_string()),
            parameters: serde_json::json!({ "type": "object" }),
        };
        assert!(
            counter.count_tools(std::slice::from_ref(&tool)) > 0,
            "工具声明要计入"
        );

        // 同一段英文：分词器给出的是真实 BPE 数，与 4 字符折算的结果不同——
        // 这正是「精确」与「估算」的分界，用它确认精确路径真的走了分词器。
        let text = "export const value = computeSomething(alpha, beta);\n";
        assert_ne!(counter.count_text(text), estimate_text(text));
    }

    /// 估算口径的实测偏差（相对官方词表）。
    ///
    /// 这里**不对估算值断言 3%**：估算器本来就不是精确的（口径见 crate 文档），
    /// 把它的偏差调成「看起来达标」才是真的谎报。偏差数值打给文档记录，
    /// `cargo test -p ai-ide-llm -- --nocapture` 可见。
    #[test]
    fn 估算口径_实测偏差如实记录() {
        for encoding in [TokenEncoding::O200kBase, TokenEncoding::Cl100kBase] {
            let bpe = encoding.bpe().expect("词表可用");
            for vector in VECTORS {
                let reference = bpe.encode_ordinary(vector).len() as u64;
                let estimated = estimate_text(vector);
                let error = (estimated as f64 - reference as f64) / reference as f64 * 100.0;
                let head: String = vector.chars().take(24).collect();
                eprintln!(
                    "[{}] 估算 {estimated} / 官方 {reference} → 偏差 {error:+.1}%（向量首段：{head}）",
                    encoding.name()
                );
            }
        }
    }

    /// 与公开示例向量交叉校验（避免「自己和自己比」的循环校验）。
    ///
    /// 前两组取自 tiktoken 官方 README 的 `cl100k_base` 示例；第三组是本次实测值，
    /// 只作回归锚点，**不声称来自官方文档**。
    #[test]
    fn 精确计数_与公开示例向量交叉校验() {
        let counter = TokenCounter::for_model("gpt-4-turbo");
        assert_eq!(counter.count_text("hello world"), 2, "官方 README 示例");
        assert_eq!(
            counter.count_text("tiktoken is great!"),
            6,
            "官方 README 示例"
        );
        // 实测锚点：接入实现对该词给出 6。将来词表或依赖升级若使它变化，这里会红，
        // 那正是需要人工确认「是正常升级还是错版」的时刻。
        assert_eq!(counter.count_text("antidisestablishmentarianism"), 6);
    }
}
