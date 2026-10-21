//! 会话导出渲染（M5 切片三）。
//!
//! 这一层是**纯函数**：会话 + 消息 → 文本，`out_path` → 最终文件路径。
//! 它不读库、不查状态，也不决定错误码——写盘失败由 `session_api` 翻成
//! `5004 EXPORT_FAILED`，会话不存在仍是 `5001`。这样「导出内容长什么样」
//! 可以脱离内核单独测，不必为改一版 Markdown 排版去起子进程。

use std::path::{Path, PathBuf};

use ai_ide_protocol::dto::{MessageView, SessionExportFormat, SessionView};
use ai_ide_protocol::ChatRole;

/// 文件名里标题的最大保留长度：太长的标题会把路径顶爆（Windows 单段 255 字节）。
const MAX_STEM_CHARS: usize = 60;

/// 按格式渲染正文。
pub fn render(
    session: &SessionView,
    messages: &[MessageView],
    format: SessionExportFormat,
    exported_at_ms: i64,
) -> String {
    match format {
        SessionExportFormat::Markdown => render_markdown(session, messages, exported_at_ms),
        SessionExportFormat::Json => {
            // 结构化导出复用协议 DTO：客户端拿到的就是 `session.get` 的那套字段，
            // 不必为「导出文件」再维护一份平行的类型。
            let document = ai_ide_protocol::dto::SessionExportDocument {
                format_version: 1,
                exported_at_ms,
                session: session.clone(),
                messages: messages.to_vec(),
            };
            serde_json::to_string_pretty(&document).unwrap_or_else(|err| {
                // 走到这里说明 DTO 自身不可序列化，属于代码缺陷；仍返回合法 JSON 字符串，
                // 避免把「序列化失败」伪装成「文件写成功但内容空白」。
                format!("{{\"format_version\":1,\"error\":\"{err}\"}}")
            })
        }
    }
}

/// 渲染 Markdown：一张会话摘要表 + 逐条消息。
///
/// 排版取舍：消息正文**原样**输出（不加代码块包裹），因为对话里本来就混着
/// Markdown / 代码 / 日志；再包一层会让粘回去的内容多出一层围栏。
pub fn render_markdown(
    session: &SessionView,
    messages: &[MessageView],
    exported_at_ms: i64,
) -> String {
    let mut out = String::with_capacity(1024 + messages.len() * 256);

    out.push_str(&format!("# {}\n\n", session.title));
    out.push_str(&format!(
        "> AI IDE 会话导出 · {} · 共 {} 条消息\n\n",
        format_utc_ms(exported_at_ms),
        messages.len()
    ));

    out.push_str("| 项目 | 值 |\n| --- | --- |\n");
    out.push_str(&format!("| 会话 ID | {} |\n", code(&session.session_id)));
    out.push_str(&format!(
        "| 工作区 | {} |\n",
        match session.workspace_path.as_deref() {
            Some(path) if !path.is_empty() => code(path),
            _ => "（未绑定工作区）".to_string(),
        }
    ));
    out.push_str(&format!(
        "| 默认模型 | {} |\n",
        match &session.model_ref {
            Some(model) => code(&format!("{}/{}", model.provider_id, model.model_id)),
            None => "（未设置）".to_string(),
        }
    ));
    out.push_str(&format!(
        "| 创建时间 | {} |\n",
        format_utc_ms(session.created_at_ms)
    ));
    out.push_str(&format!(
        "| 最后活动 | {} |\n",
        format_utc_ms(session.updated_at_ms)
    ));
    out.push_str(&format!("| 消息数 | {} |\n", messages.len()));

    if messages.is_empty() {
        out.push_str("\n---\n\n_（该会话还没有消息）_\n");
        return out;
    }

    for (index, message) in messages.iter().enumerate() {
        out.push_str(&format!(
            "\n---\n\n## {} · {} · {}\n\n",
            index + 1,
            role_name(message.role),
            format_utc_ms(message.created_at_ms)
        ));

        if message.content.trim().is_empty() {
            out.push_str("_（空内容）_\n");
        } else {
            out.push_str(message.content.trim_end());
            out.push('\n');
        }

        if !message.attachments.is_empty() {
            out.push_str("\n**附件**\n\n");
            for attachment in &message.attachments {
                out.push_str(&format!(
                    "- {} · {}\n",
                    code(&attachment.path),
                    attachment.kind
                ));
            }
        }

        if !message.tool_calls.is_empty() {
            out.push_str("\n**工具调用**\n\n");
            for call in &message.tool_calls {
                out.push_str(&format!("- {}（`{}`）\n", code(&call.name), call.id));
                if !call.arguments.trim().is_empty() {
                    out.push_str("\n```json\n");
                    out.push_str(call.arguments.trim());
                    out.push_str("\n```\n");
                }
            }
        }

        if let Some(tool_call_id) = message.tool_call_id.as_deref() {
            out.push_str(&format!("\n> 回应工具调用 `{tool_call_id}`\n"));
        }

        if let Some(model) = &message.model_ref {
            out.push_str(&format!(
                "\n_模型：{}/{}_\n",
                model.provider_id, model.model_id
            ));
        }
    }

    out
}

/// 把 `out_path` 解析成最终文件路径。
///
/// 规则（写进文档，别让调用方猜）：
/// - 必须是绝对路径：进程 cwd 属于内核安装目录，相对路径会悄悄落到用户找不到的地方；
/// - 指向**已存在的目录**时，在里面按「标题-会话短 id」生成文件名；
/// - 其余情况当作文件路径，没写扩展名就按格式补 `.md` / `.json`。
pub fn resolve_out_path(
    raw: &str,
    format: SessionExportFormat,
    session: &SessionView,
) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("out_path 不能为空".to_string());
    }

    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(format!("out_path 必须是绝对路径：{trimmed}"));
    }

    if path.is_dir() {
        return Ok(path.join(default_file_name(session, format)));
    }
    if path.extension().is_none() {
        return Ok(path.with_extension(format.extension()));
    }
    Ok(path)
}

/// 写盘：父目录不存在就建出来；返回写入字节数。
pub fn write(path: &Path, content: &str) -> std::io::Result<u64> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, content)?;
    Ok(content.len() as u64)
}

/// 目录入参下生成的文件名：`<标题>-<id 前 8 位>.<ext>`。
fn default_file_name(session: &SessionView, format: SessionExportFormat) -> String {
    let stem = sanitize_stem(&session.title);
    let short_id: String = session.session_id.chars().take(8).collect();
    if short_id.is_empty() {
        format!("{stem}.{}", format.extension())
    } else {
        format!("{stem}-{short_id}.{}", format.extension())
    }
}

/// 标题 → 合法文件名片段：去掉路径分隔符与 Windows 保留字符，空白折成单个 `-`。
fn sanitize_stem(title: &str) -> String {
    let mut stem = String::with_capacity(title.len());
    let mut last_dash = false;
    for ch in title.chars() {
        let replaced = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            ch if ch.is_control() => '-',
            ch if ch.is_whitespace() => '-',
            ch => ch,
        };
        if replaced == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        stem.push(replaced);
        if stem.chars().count() >= MAX_STEM_CHARS {
            break;
        }
    }
    let stem = stem.trim_matches(['-', '.'].as_slice()).to_string();
    if stem.is_empty() {
        "会话".to_string()
    } else {
        stem
    }
}

fn role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

/// 行内代码：内容里有反引号时退回纯文本，避免把后面的整段吞进代码块。
fn code(text: &str) -> String {
    if text.contains('`') {
        text.replace('|', "\\|")
    } else {
        format!("`{}`", text.replace('|', "\\|"))
    }
}

/// Unix 毫秒 → `YYYY-MM-DD HH:MM:SS UTC`。
///
/// 不引 `chrono`：内核只导出这一个时间格式，为了它多背一个依赖不值；
/// 逆算用 Howard Hinnant 的 civil_from_days，公历闰年规则已覆盖到 4000 年。
pub fn format_utc_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let millis_of_day = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);

    let total_seconds = millis_of_day / 1000;
    let hour = total_seconds / 3600;
    let minute = (total_seconds % 3600) / 60;
    let second = total_seconds % 60;

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153; // [0, 11]，3 月为 0
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_protocol::dto::{ChatAttachment, SessionExportFormat};
    use ai_ide_protocol::{ChatToolCall, ModelRef};

    fn session() -> SessionView {
        SessionView {
            session_id: "12345678-abcd".into(),
            title: "仓库导览 / 排查: 编译".into(),
            workspace_path: Some("E:/ai_ide".into()),
            model_ref: Some(ModelRef {
                provider_id: "openai".into(),
                model_id: "gpt-4o".into(),
            }),
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_100_000,
        }
    }

    fn message(index: u32) -> MessageView {
        MessageView {
            message_id: format!("m{index}"),
            session_id: "12345678-abcd".into(),
            parent_id: None,
            role: ChatRole::User,
            content: format!("第 {index} 条：`code` 与 | 竖线"),
            attachments: vec![ChatAttachment {
                kind: "file".into(),
                path: "src/main.rs".into(),
            }],
            tool_calls: vec![ChatToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"src/main.rs\"}".into(),
            }],
            tool_call_id: None,
            model_ref: None,
            usage: None,
            created_at_ms: 1_700_000_000_000 + i64::from(index) * 1000,
        }
    }

    #[test]
    fn formats_utc_timestamps_without_external_deps() {
        assert_eq!(format_utc_ms(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc_ms(1_700_000_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(
            format_utc_ms(1_700_000_000_000 + 86_400_000),
            "2023-11-15 22:13:20 UTC",
            "跨日必须进位，不能靠小时除以 24 硬算"
        );
    }

    #[test]
    fn markdown_contains_summary_and_every_message() {
        let messages = vec![message(1), message(2)];
        let text = render_markdown(&session(), &messages, 1_700_000_200_000);

        assert!(text.starts_with("# 仓库导览 / 排查: 编译"));
        assert!(text.contains("| 会话 ID | `12345678-abcd` |"));
        assert!(text.contains("| 默认模型 | `openai/gpt-4o` |"));
        assert!(text.contains("| 消息数 | 2 |"));
        assert!(text.contains("## 1 · user · 2023-11-14 22:13:21 UTC"));
        assert!(text.contains("## 2 · user · 2023-11-14 22:13:22 UTC"));
        assert!(text.contains("- `src/main.rs` · file"));
        assert!(text.contains("```json"));
    }

    #[test]
    fn markdown_marks_empty_conversation() {
        let text = render_markdown(&session(), &[], 1_700_000_200_000);
        assert!(text.contains("_（该会话还没有消息）_"));
    }

    #[test]
    fn json_export_matches_protocol_dto() {
        let text = render(
            &session(),
            &[message(1)],
            SessionExportFormat::Json,
            1_700_000_200_000,
        );
        let value: serde_json::Value = serde_json::from_str(&text).expect("必须是合法 JSON");
        assert_eq!(value["format_version"], serde_json::json!(1));
        assert_eq!(
            value["session"]["session_id"],
            serde_json::json!("12345678-abcd")
        );
        assert_eq!(value["messages"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn out_path_must_be_absolute() {
        let err = resolve_out_path("导出.md", SessionExportFormat::Markdown, &session())
            .expect_err("相对路径必须被拒绝");
        assert!(err.contains("绝对路径"));
    }

    #[test]
    fn out_path_fills_extension_and_sanitizes_title() {
        let path = resolve_out_path(r"E:\ai_ide\导出", SessionExportFormat::Markdown, &session())
            .expect("绝对路径");
        assert_eq!(path, PathBuf::from(r"E:\ai_ide\导出.md"));

        let explicit = resolve_out_path(
            r"E:\ai_ide\导出.json",
            SessionExportFormat::Json,
            &session(),
        )
        .expect("绝对路径");
        assert_eq!(explicit, PathBuf::from(r"E:\ai_ide\导出.json"));

        let name = default_file_name(&session(), SessionExportFormat::Json);
        assert_eq!(name, "仓库导览-排查-编译-12345678.json");
    }
}
