//! `edit`：把文件里的一个片段替换成另一个片段。
//!
//! 这个工具的语义只有一条要点：**`old_text` 必须在文件里唯一出现**。
//! 找不到、或者出现多次，都算失败——但失败是**可回灌**的：模型看到
//! 「出现 3 次，请补足上下文」会自己把锚点写长一点再试。M4 验收里
//! 「`edit_file` 非唯一匹配自动重试成功率 ≥ 90%」就建立在这条通道上。

use super::Args;
use crate::error::AgentError;
use crate::registry::{Tool, ToolContext, ToolOutcome};
use ai_ide_workspace::fs_ops;
use serde_json::{json, Value};

const NAME: &str = "edit";

/// 编辑后的内容上限：与 `read` 的字节口径一致（256 KiB）。
const MAX_BYTES: usize = 256 * 1024;

/// 唯一匹配替换。
pub struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &'static str {
        "把文件里的 old_text 替换成 new_text。old_text 必须在文件里**唯一**出现：找不到或出现多次都会失败，请补足上下文让锚点唯一。可传 expected_hash 做乐观锁。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区根的路径" },
                "old_text": { "type": "string", "description": "要被替换的原文，必须唯一匹配" },
                "new_text": { "type": "string", "description": "替换后的内容，允许空串（表示删除）" },
                "expected_hash": { "type": "string", "description": "磁盘当前的 SHA-256；给了就做乐观锁校验" }
            },
            "required": ["path", "old_text", "new_text"],
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let path = args.str("path")?;
        let old_text = args.text("old_text")?;
        let new_text = args.text("new_text")?;
        let expected_hash = args.opt_str("expected_hash")?;

        if old_text.is_empty() {
            return Err(AgentError::BadArgs {
                tool: NAME.to_string(),
                reason: "参数 `old_text` 不能为空：空串会匹配任意位置，无法定位".into(),
            });
        }

        // 闸门先行。
        let abs = ctx.guard.resolve_for_write(path)?;
        let relative = ctx.display_path(&abs);
        if !abs.is_file() {
            return Err(AgentError::ToolFailed {
                tool: NAME.to_string(),
                message: format!("文件不存在：{relative}"),
            });
        }

        let file = fs_ops::read_file(ctx.guard, path, None, None)?;
        let matches = count_occurrences(&file.content, old_text);
        match matches {
            0 => {
                return Err(AgentError::Conflict {
                    tool: NAME.to_string(),
                    message: format!(
                        "在 {relative} 里找不到 old_text。请先用 read 确认原文（注意缩进与换行）"
                    ),
                });
            }
            1 => {}
            count => {
                return Err(AgentError::Conflict {
                    tool: NAME.to_string(),
                    message: format!(
                        "old_text 在 {relative} 里出现了 {count} 次，无法唯一匹配。请把 old_text 补足到唯一（含上下文行）后重试"
                    ),
                });
            }
        }

        let updated = file.content.replacen(old_text, new_text, 1);
        if updated == file.content {
            return Err(AgentError::ToolFailed {
                tool: NAME.to_string(),
                message: format!("替换后内容与原文相同（{relative}），未做改动"),
            });
        }
        let bytes = updated.len();
        if bytes > MAX_BYTES {
            return Err(AgentError::ToolFailed {
                tool: NAME.to_string(),
                message: format!(
                    "替换后为 {bytes} 字节，超过单文件上限 {MAX_BYTES} 字节；请分多次小改动完成"
                ),
            });
        }

        let written = fs_ops::write_file(ctx.guard, path, &updated, expected_hash, None)?;
        Ok(ToolOutcome::new(
            json!({
                "ok": true,
                "path": relative,
                "replaced": 1,
                "bytes": bytes,
                "hash": written.hash,
            })
            .to_string(),
        ))
    }
}

/// 非重叠子串计数。
///
/// 不用 `str::matches`：它把空模式算作「每个位置都命中」，而空 `old_text` 已经在
/// 上游被挡掉，这里显式返回 0 更省心。
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut rest = haystack;
    while let Some(index) = rest.find(needle) {
        count += 1;
        rest = &rest[index + needle.len()..];
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, Sandbox};
    use ai_ide_protocol::ErrorCode;

    #[test]
    fn replaces_a_unique_snippet() {
        let sandbox = Sandbox::new("edit-basic");
        sandbox.write("a.ts", "const a = 1;\nconst b = 2;\n");

        let out = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "const a = 1;", "new_text": "const a = 42;"}),
        )
        .unwrap();
        assert!(out.content.contains("\"replaced\":1"));
        assert_eq!(sandbox.read("a.ts"), "const a = 42;\nconst b = 2;\n");
    }

    #[test]
    fn multi_line_snippet_works() {
        let sandbox = Sandbox::new("edit-multiline");
        sandbox.write("a.ts", "fn one() {}\nfn two() {}\n");

        call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "fn one() {}\nfn two() {}", "new_text": "fn only() {}"}),
        )
        .unwrap();
        assert_eq!(sandbox.read("a.ts"), "fn only() {}\n");
    }

    #[test]
    fn ambiguous_match_fails_and_lists_the_count() {
        let sandbox = Sandbox::new("edit-ambiguous");
        let original = "let x = 1;\nlet x = 1;\nlet x = 1;\n";
        sandbox.write("a.ts", original);

        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "let x = 1;", "new_text": "let x = 2;"}),
        )
        .unwrap_err();
        assert!(
            !err.is_fatal(),
            "非唯一匹配必须可回灌，模型才能补足上下文重试"
        );
        assert_eq!(err.code(), ErrorCode::DIFF_APPLY_CONFLICT, "复用 3003");
        assert!(err.to_string().contains("出现了 3 次"));
        assert_eq!(sandbox.read("a.ts"), original, "失败时不能动文件");
    }

    #[test]
    fn retry_with_a_longer_anchor_succeeds() {
        // 这正是验收里「非唯一匹配重试」的最小形态：先失败，补足上下文后成功。
        let sandbox = Sandbox::new("edit-retry");
        sandbox.write(
            "a.ts",
            "fn a() {\n  let x = 1;\n}\nfn b() {\n  let x = 1;\n}\n",
        );

        let first = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "let x = 1;", "new_text": "let x = 2;"}),
        );
        assert!(first.is_err());

        let second = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "fn b() {\n  let x = 1;\n}", "new_text": "fn b() {\n  let x = 2;\n}"}),
        );
        assert!(second.is_ok(), "补足上下文后应唯一匹配");
        assert!(sandbox.read("a.ts").contains("fn b() {\n  let x = 2;\n}"));
        assert!(sandbox.read("a.ts").contains("fn a() {\n  let x = 1;\n}"));
    }

    #[test]
    fn missing_snippet_is_recoverable() {
        let sandbox = Sandbox::new("edit-missing");
        sandbox.write("a.ts", "const a = 1;\n");

        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "const z = 9;", "new_text": "x"}),
        )
        .unwrap_err();
        assert!(!err.is_fatal());
        assert_eq!(err.code(), ErrorCode::DIFF_APPLY_CONFLICT);
        assert!(err.to_string().contains("找不到"));
        assert_eq!(sandbox.read("a.ts"), "const a = 1;\n");
    }

    #[test]
    fn empty_old_text_is_bad_args() {
        let sandbox = Sandbox::new("edit-emptyold");
        sandbox.write("a.ts", "const a = 1;\n");

        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "", "new_text": "x"}),
        )
        .unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert_eq!(sandbox.read("a.ts"), "const a = 1;\n");
    }

    #[test]
    fn empty_new_text_deletes_the_snippet() {
        let sandbox = Sandbox::new("edit-delete");
        sandbox.write("a.ts", "keep\ndrop\n");

        call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "drop\n", "new_text": ""}),
        )
        .unwrap();
        assert_eq!(sandbox.read("a.ts"), "keep\n");
    }

    #[test]
    fn identical_replacement_is_rejected() {
        let sandbox = Sandbox::new("edit-noop");
        sandbox.write("a.ts", "const a = 1;\n");

        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "const a = 1;", "new_text": "const a = 1;"}),
        )
        .unwrap_err();
        assert!(!err.is_fatal());
        assert!(err.to_string().contains("未做改动"));
    }

    #[test]
    fn missing_file_is_recoverable() {
        let sandbox = Sandbox::new("edit-nofile");
        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "nope.ts", "old_text": "a", "new_text": "b"}),
        )
        .unwrap_err();
        assert!(!err.is_fatal());
        assert!(err.to_string().contains("文件不存在"));
    }

    #[test]
    fn stale_hash_is_rejected_without_touching_disk() {
        let sandbox = Sandbox::new("edit-lock");
        sandbox.write("a.ts", "const a = 1;\n");

        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "const a = 1;", "new_text": "const a = 2;", "expected_hash": "deadbeef"}),
        )
        .unwrap_err();
        assert!(!err.is_fatal());
        assert_eq!(sandbox.read("a.ts"), "const a = 1;\n");
    }

    #[test]
    fn oversized_payload_is_rejected_before_writing() {
        let sandbox = Sandbox::new("edit-oversize");
        sandbox.write("a.ts", "anchor\n");

        let huge = "x".repeat(MAX_BYTES + 1);
        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "a.ts", "old_text": "anchor", "new_text": huge}),
        )
        .unwrap_err();
        assert!(!err.is_fatal());
        assert!(err.to_string().contains("超过单文件上限"));
        assert_eq!(sandbox.read("a.ts"), "anchor\n");
    }

    #[test]
    fn out_of_workspace_path_is_fatal() {
        let sandbox = Sandbox::new("edit-escape");
        let err = call(
            &EditTool,
            &sandbox,
            json!({"path": "../a.ts", "old_text": "a", "new_text": "b"}),
        )
        .unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    }

    #[test]
    fn counts_only_non_overlapping_occurrences() {
        assert_eq!(count_occurrences("aaaa", "aa"), 2);
        assert_eq!(count_occurrences("abc", "z"), 0);
        assert_eq!(count_occurrences("abc", ""), 0);
    }
}
