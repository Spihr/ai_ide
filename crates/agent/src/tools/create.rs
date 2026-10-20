//! `create`：新建文件或目录（不写内容）。

use super::Args;
use crate::error::AgentError;
use crate::registry::{Tool, ToolContext, ToolOutcome};
use ai_ide_protocol::CreateKind;
use ai_ide_workspace::fs_ops;
use serde_json::{json, Value};

const NAME: &str = "create";

/// 新建文件或目录。
///
/// 与 `write` 的分工：`write` 是「把内容放进去」，`create` 是「先把位置占下来」。
/// 目标已存在时报错而不是静默通过——模型以为在新建、实际覆盖了别人的东西是最坏的情况。
pub struct CreateTool;

impl Tool for CreateTool {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &'static str {
        "在工作区里新建文件或目录（父目录自动补齐），已存在则报错。只建位置，不写内容；要写内容请用 write。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区根的路径" },
                "kind": { "type": "string", "enum": ["file", "dir"], "description": "新建类型，默认 file" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let path = args.str("path")?;
        let kind = parse_kind(args.opt_str("kind")?)?;

        // 闸门先行：越界、敏感路径在这里被拒，不落任何东西。
        let abs = ctx.guard.resolve_for_write(path)?;
        let relative = ctx.display_path(&abs);

        fs_ops::create(ctx.guard, path, kind)?;
        Ok(ToolOutcome::new(
            json!({
                "ok": true,
                "path": relative,
                "kind": match kind {
                    CreateKind::File => "file",
                    CreateKind::Dir => "dir",
                },
            })
            .to_string(),
        ))
    }
}

fn parse_kind(raw: Option<&str>) -> Result<CreateKind, AgentError> {
    match raw {
        None | Some("file") => Ok(CreateKind::File),
        Some("dir") | Some("directory") => Ok(CreateKind::Dir),
        Some(other) => Err(AgentError::BadArgs {
            tool: NAME.to_string(),
            reason: format!("参数 `kind` 只能是 file 或 dir（收到 `{other}`）"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, Sandbox};
    use ai_ide_protocol::ErrorCode;

    #[test]
    fn creates_a_file() {
        let sandbox = Sandbox::new("create-file");
        let out = call(&CreateTool, &sandbox, json!({"path": "a.txt"})).unwrap();
        assert!(out.content.contains("\"ok\":true"));
        assert!(out.content.contains("\"kind\":\"file\""));
        assert!(sandbox.exists("a.txt"));
        assert_eq!(sandbox.read("a.txt"), "");
    }

    #[test]
    fn creates_a_directory_with_parents() {
        let sandbox = Sandbox::new("create-dir");
        let out = call(
            &CreateTool,
            &sandbox,
            json!({"path": "src/deep", "kind": "dir"}),
        )
        .unwrap();
        assert!(out.content.contains("\"kind\":\"dir\""));
        assert!(sandbox.path("src/deep").is_dir());
    }

    #[test]
    fn existing_target_is_recoverable() {
        let sandbox = Sandbox::new("create-exists");
        sandbox.write("a.txt", "keep me\n");

        let err = call(&CreateTool, &sandbox, json!({"path": "a.txt"})).unwrap_err();
        assert!(
            !err.is_fatal(),
            "目标已存在应回灌，让模型改用 write 或换个名字"
        );
        assert_eq!(sandbox.read("a.txt"), "keep me\n");
    }

    #[test]
    fn unknown_kind_is_bad_args() {
        let sandbox = Sandbox::new("create-badkind");
        let err = call(
            &CreateTool,
            &sandbox,
            json!({"path": "a.txt", "kind": "symlink"}),
        )
        .unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert!(!sandbox.exists("a.txt"));
    }

    #[test]
    fn sensitive_path_is_fatal() {
        let sandbox = Sandbox::new("create-secret");
        let err = call(&CreateTool, &sandbox, json!({"path": ".env"})).unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_DENIED);
        assert!(!sandbox.exists(".env"));
    }

    #[test]
    fn out_of_workspace_path_is_fatal() {
        let sandbox = Sandbox::new("create-escape");
        let err = call(&CreateTool, &sandbox, json!({"path": "../x.txt"})).unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    }
}
