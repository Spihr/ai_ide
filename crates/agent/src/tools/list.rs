//! `list`：列出目录条目。

use super::Args;
use crate::error::AgentError;
use crate::message::escape_xml;
use crate::registry::{Tool, ToolContext, ToolOutcome};
use ai_ide_protocol::{EntryKind, TreeEntry};
use ai_ide_workspace::tree;
use serde_json::{json, Value};

const NAME: &str = "list";

/// 列出目录。
///
/// 直接复用 M2 的 `workspace.tree`：同一套忽略规则（`.gitignore` + 默认忽略表）、
/// 同一套排序（目录优先、再按名称不区分大小写），UI 与 Agent 看到的是同一份世界。
pub struct ListTool;

impl Tool for ListTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "列出工作区内一个目录的条目（目录优先、再按名称排序）。默认只列一层，depth 可递归，上限 8 层；忽略规则命中的条目会被过滤。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区根的目录路径；缺省为根目录" },
                "depth": { "type": "integer", "minimum": 1, "maximum": tree::MAX_DEPTH, "description": "递归层数，默认 1" },
                "include_ignored": { "type": "boolean", "description": "是否连忽略命中的条目一起返回，默认 false" }
            },
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let path = args.opt_str("path")?;
        let depth = args
            .opt_u32("depth")?
            .unwrap_or(1)
            .clamp(1, tree::MAX_DEPTH);
        let include_ignored = args.opt_bool("include_ignored")?.unwrap_or(false);

        // 闸门先行：越界 / 敏感路径在这里终止循环。
        let abs = match path {
            Some(input) => ctx.guard.resolve(input)?,
            None => ctx.root.to_path_buf(),
        };
        if !abs.is_dir() {
            return Err(AgentError::ToolFailed {
                tool: NAME.to_string(),
                message: if abs.exists() {
                    format!("{} 是文件，不是目录", ctx.display_path(&abs))
                } else {
                    format!("目录不存在：{}", ctx.display_path(&abs))
                },
            });
        }

        let relative = ctx.display_path(&abs);
        let entries = tree::list(ctx.guard, path, depth, include_ignored)?;
        Ok(ToolOutcome::new(render(&relative, depth, &entries)))
    }
}

/// 每行一个条目标签：`kind<TAB>name<TAB>size[<TAB>ignored]`。
///
/// 用制表符而不是嵌套 XML：目录深了以后嵌套标签的 token 开销很可观，而扁平行
/// 对模型同样好读，路径信息也不会丢（目录行以 `/` 结尾）。
fn render(relative: &str, depth: u32, entries: &[TreeEntry]) -> String {
    let mut out = format!(
        "<tree path=\"{}\" depth=\"{depth}\" entries=\"{}\">\n",
        escape_xml(relative),
        entries.len()
    );
    if entries.is_empty() {
        out.push_str("（空目录）\n");
    }
    for entry in entries {
        let kind = kind_name(entry.kind);
        out.push_str(kind);
        out.push('\t');
        out.push_str(&escape_xml(&entry.name));
        if matches!(entry.kind, EntryKind::Dir) {
            out.push('/');
        }
        out.push('\t');
        out.push_str(&entry.size.to_string());
        if entry.ignored {
            out.push_str("\tignored");
        }
        out.push('\n');
    }
    out.push_str("</tree>\n");
    out
}

fn kind_name(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, Sandbox};

    #[test]
    fn lists_the_workspace_root() {
        let sandbox = Sandbox::new("list-root");
        sandbox.write("src/a.ts", "export {}\n");
        sandbox.write("readme.md", "# hi\n");

        let out = call(&ListTool, &sandbox, json!({})).unwrap();
        assert!(out
            .content
            .starts_with("<tree path=\".\" depth=\"1\" entries=\"2\">"));
        // 目录优先：src 应排在 readme.md 前面。
        let src = out.content.find("dir\tsrc/").expect("应列出 src");
        let readme = out.content.find("readme.md").expect("应列出 readme.md");
        assert!(src < readme, "目录应排在文件之前");
    }

    #[test]
    fn one_level_by_default() {
        let sandbox = Sandbox::new("list-depth");
        sandbox.write("src/deep/a.ts", "x\n");

        let shallow = call(&ListTool, &sandbox, json!({})).unwrap();
        assert!(shallow.content.contains("src/"));
        assert!(!shallow.content.contains("deep/"));

        let deep = call(&ListTool, &sandbox, json!({"depth": 3})).unwrap();
        assert!(deep.content.contains("deep/"));
        assert!(deep.content.contains("a.ts"));
    }

    #[test]
    fn depth_is_clamped_to_the_documented_maximum() {
        let sandbox = Sandbox::new("list-clamp");
        let out = call(&ListTool, &sandbox, json!({"depth": 99})).unwrap();
        assert!(out
            .content
            .contains(&format!("depth=\"{}\"", tree::MAX_DEPTH)));
    }

    #[test]
    fn ignored_entries_are_hidden_unless_asked_for() {
        let sandbox = Sandbox::new("list-ignored");
        sandbox.write("keep.txt", "1\n");
        sandbox.write("node_modules/dep/index.js", "2\n");

        let hidden = call(&ListTool, &sandbox, json!({})).unwrap();
        assert!(hidden.content.contains("keep.txt"));
        assert!(!hidden.content.contains("node_modules"));

        let shown = call(
            &ListTool,
            &sandbox,
            json!({"include_ignored": true, "depth": 2}),
        )
        .unwrap();
        assert!(shown.content.contains("node_modules"));
        assert!(shown.content.contains("ignored"));
    }

    #[test]
    fn empty_directory_is_reported_as_such() {
        let sandbox = Sandbox::new("list-empty");
        std::fs::create_dir_all(sandbox.path("empty")).unwrap();

        let out = call(&ListTool, &sandbox, json!({"path": "empty"})).unwrap();
        assert!(out.content.contains("entries=\"0\""));
        assert!(out.content.contains("（空目录）"));
    }

    #[test]
    fn a_file_is_not_a_directory() {
        let sandbox = Sandbox::new("list-file");
        sandbox.write("a.txt", "x\n");

        let err = call(&ListTool, &sandbox, json!({"path": "a.txt"})).unwrap_err();
        assert!(!err.is_fatal());
        assert!(err.to_string().contains("不是目录"));
    }

    #[test]
    fn out_of_workspace_path_is_fatal() {
        let sandbox = Sandbox::new("list-escape");
        let err = call(&ListTool, &sandbox, json!({"path": "../../.."})).unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(
            err.code(),
            ai_ide_protocol::ErrorCode::PATH_OUT_OF_WORKSPACE
        );
    }

    #[test]
    fn bad_depth_type_is_bad_args() {
        let sandbox = Sandbox::new("list-baddepth");
        let err = call(&ListTool, &sandbox, json!({"depth": "two"})).unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert!(!err.is_fatal());
    }
}
