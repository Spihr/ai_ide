//! `read`：读取工作区内文件内容。

use super::Args;
use crate::error::AgentError;
use crate::registry::{Tool, ToolContext, ToolOutcome};
use ai_ide_protocol::WorkspaceReadFileResult;
use ai_ide_workspace::fs_ops;
use serde_json::{json, Value};

const NAME: &str = "read";

/// 单次读取的字节上限。与 M3 上下文注入同口径（`core::context::SINGLE_FILE_MAX_BYTES`）。
pub const MAX_BYTES: u64 = 256 * 1024;

/// 单次读取的行数上限。与 M3 上下文注入同口径（`core::context::SINGLE_FILE_MAX_LINES`）。
pub const MAX_LINES: usize = 2_000;

/// 读取文件。
pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &'static str {
        "读取工作区内一个文件，返回带行号的文本。单次最多 2000 行 / 256 KiB，超出会在标签上标 truncated=lines|bytes。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区根的路径" },
                "start_line": { "type": "integer", "minimum": 1, "description": "起始行（1 起、含）" },
                "end_line": { "type": "integer", "minimum": 1, "description": "结束行（1 起、含）" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let path = args.str("path")?;
        let start_line = args.opt_u32("start_line")?;
        let end_line = args.opt_u32("end_line")?;

        // 闸门先行：越界 / 敏感路径在这里终止整个循环，不降级成「读不到」。
        let abs = ctx.guard.resolve(path)?;
        let relative = ctx.display_path(&abs);
        if !abs.is_file() {
            return Err(AgentError::ToolFailed {
                tool: NAME.to_string(),
                message: if abs.exists() {
                    format!("{relative} 是目录，不是文件")
                } else {
                    format!("文件不存在：{relative}")
                },
            });
        }

        let file = fs_ops::read_file(ctx.guard, path, None, Some(MAX_BYTES))?;
        render(&relative, &file, start_line, end_line).map(ToolOutcome::new)
    }
}

/// 渲染成 `<file ...>` 块。
///
/// 形态与 M3 的 `@文件` 注入保持一致，模型不必区分「用户给的文件」与「自己读到的文件」；
/// 行号前缀让 `edit` 的 `old_text` 能直接照着抄。
fn render(
    path: &str,
    file: &WorkspaceReadFileResult,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> Result<String, AgentError> {
    let lines: Vec<&str> = file.content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Ok(format!("<file path=\"{path}\" lines=\"0-0\" />\n"));
    }

    let start = start_line.unwrap_or(1).max(1) as usize;
    if start > total {
        return Err(AgentError::ToolFailed {
            tool: NAME.to_string(),
            message: format!("start_line={start} 超出文件范围（{path} 共 {total} 行）"),
        });
    }

    // 字节截断由 fs_ops 完成；行数上限在这里收口，两者取更窄的那个。
    let mut truncated = if file.truncated { Some("bytes") } else { None };
    let requested_end = end_line.map(|v| v as usize).unwrap_or(total).max(start);
    let end = requested_end.min(total);
    let mut last = end;
    if end - start + 1 > MAX_LINES {
        truncated = Some("lines");
        last = start + MAX_LINES - 1;
    }

    let mut out = format!("<file path=\"{path}\" lines=\"{start}-{last}\"");
    if let Some(kind) = truncated {
        out.push_str(&format!(" truncated=\"{kind}\""));
    }
    out.push_str(">\n");
    for (offset, line) in lines[start - 1..=last - 1].iter().enumerate() {
        out.push_str(&format!("{}| {}\n", start + offset, line));
    }
    out.push_str("</file>\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, Sandbox};

    fn numbered(total: usize) -> String {
        (1..=total).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn renders_numbered_lines() {
        let sandbox = Sandbox::new("read-basic");
        sandbox.write("a.txt", "alpha\nbeta\n");

        let out = call(&ReadTool, &sandbox, json!({"path": "a.txt"})).unwrap();
        assert!(out
            .content
            .starts_with("<file path=\"a.txt\" lines=\"1-2\">"));
        assert!(out.content.contains("1| alpha"));
        assert!(out.content.contains("2| beta"));
        assert!(out.content.ends_with("</file>\n"));
        assert!(!out.content.contains("truncated"));
    }

    #[test]
    fn honours_the_requested_range() {
        let sandbox = Sandbox::new("read-range");
        sandbox.write("a.txt", &numbered(10));

        let out = call(
            &ReadTool,
            &sandbox,
            json!({"path": "a.txt", "start_line": 3, "end_line": 5}),
        )
        .unwrap();
        assert!(out.content.contains("lines=\"3-5\""));
        assert!(out.content.contains("3| line 3"));
        assert!(out.content.contains("5| line 5"));
        assert!(!out.content.contains("line 6"));
    }

    #[test]
    fn exactly_at_line_limit_is_not_truncated() {
        let sandbox = Sandbox::new("read-exact");
        sandbox.write("a.txt", &numbered(MAX_LINES));

        let out = call(&ReadTool, &sandbox, json!({"path": "a.txt"})).unwrap();
        assert!(out.content.contains(&format!("lines=\"1-{MAX_LINES}\"")));
        assert!(
            !out.content.contains("truncated"),
            "正好 2000 行是边界内的，不该标截断"
        );
    }

    #[test]
    fn truncates_at_line_limit() {
        let sandbox = Sandbox::new("read-lines");
        sandbox.write("a.txt", &numbered(MAX_LINES + 1));

        let out = call(&ReadTool, &sandbox, json!({"path": "a.txt"})).unwrap();
        assert!(out.content.contains("truncated=\"lines\""));
        assert!(out.content.contains(&format!("lines=\"1-{MAX_LINES}\"")));
        assert!(!out
            .content
            .contains(&format!("{}| line {}", MAX_LINES + 1, MAX_LINES + 1)));
    }

    #[test]
    fn truncates_at_byte_limit() {
        let sandbox = Sandbox::new("read-bytes");
        // 单行 300 KiB：超过 256 KiB 的字节上限，但远没到 2000 行。
        sandbox.write("big.txt", &"a".repeat(300 * 1024));

        let out = call(&ReadTool, &sandbox, json!({"path": "big.txt"})).unwrap();
        assert!(out.content.contains("truncated=\"bytes\""));
        assert!(out.content.contains("lines=\"1-1\""));
    }

    #[test]
    fn empty_file_renders_self_closing_block() {
        let sandbox = Sandbox::new("read-empty");
        sandbox.write("empty.txt", "");

        let out = call(&ReadTool, &sandbox, json!({"path": "empty.txt"})).unwrap();
        assert!(out.content.contains("lines=\"0-0\""));
    }

    #[test]
    fn missing_file_is_recoverable() {
        let sandbox = Sandbox::new("read-missing");
        let err = call(&ReadTool, &sandbox, json!({"path": "nope.txt"})).unwrap_err();
        assert!(matches!(err, AgentError::ToolFailed { .. }));
        assert!(!err.is_fatal(), "读不到文件应回灌给模型");
    }

    #[test]
    fn directory_is_not_a_file() {
        let sandbox = Sandbox::new("read-dir");
        std::fs::create_dir_all(sandbox.path("src")).unwrap();

        let err = call(&ReadTool, &sandbox, json!({"path": "src"})).unwrap_err();
        assert!(!err.is_fatal());
        assert!(err.to_string().contains("是目录"));
    }

    #[test]
    fn out_of_workspace_path_is_fatal_with_1002() {
        let sandbox = Sandbox::new("read-escape");
        let err = call(
            &ReadTool,
            &sandbox,
            json!({"path": "../../../Windows/System32/drivers/etc/hosts"}),
        )
        .unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(
            err.code(),
            ai_ide_protocol::ErrorCode::PATH_OUT_OF_WORKSPACE
        );
    }

    #[test]
    fn start_line_beyond_end_of_file_is_recoverable() {
        let sandbox = Sandbox::new("read-oob");
        sandbox.write("a.txt", "alpha\n");

        let err = call(
            &ReadTool,
            &sandbox,
            json!({"path": "a.txt", "start_line": 9}),
        )
        .unwrap_err();
        assert!(matches!(err, AgentError::ToolFailed { .. }));
        assert!(!err.is_fatal());
    }

    #[test]
    fn missing_path_argument_is_bad_args() {
        let sandbox = Sandbox::new("read-noargs");
        let err = call(&ReadTool, &sandbox, json!({})).unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert!(!err.is_fatal());
    }
}
