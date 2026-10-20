//! `write`：整体写入（覆盖）工作区内的文件。

use super::Args;
use crate::error::AgentError;
use crate::registry::{Tool, ToolContext, ToolOutcome};
use ai_ide_workspace::fs_ops;
use serde_json::{json, Value};

const NAME: &str = "write";

/// 整体写入。
///
/// 复用 `fs_ops::write_file`：原子写 + 乐观锁 + 沿用原文件编码与 EOL。
/// 父目录不存在会自动补齐，所以模型不必先 `create` 目录。
pub struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &'static str {
        "整体写入（覆盖）工作区内一个文件，父目录自动创建。可传 expected_hash 做乐观锁：与磁盘当前内容不一致即拒绝写入。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区根的路径" },
                "content": { "type": "string", "description": "完整的新内容（允许空串）" },
                "expected_hash": { "type": "string", "description": "磁盘当前的 SHA-256；给了就做乐观锁校验" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let path = args.str("path")?;
        let content = args.text("content")?;
        let expected_hash = args.opt_str("expected_hash")?;

        // 闸门先行：越界、敏感路径（.env / .git / 密钥）在这里被拒，且不写任何东西。
        let abs = ctx.guard.resolve_for_write(path)?;
        let relative = ctx.display_path(&abs);

        let written = fs_ops::write_file(ctx.guard, path, content, expected_hash, None)?;
        Ok(ToolOutcome::new(
            json!({
                "ok": true,
                "path": relative,
                "hash": written.hash,
                "bytes": content.len(),
            })
            .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, Sandbox};
    use ai_ide_protocol::ErrorCode;

    #[test]
    fn writes_content_and_returns_hash() {
        let sandbox = Sandbox::new("write-basic");
        sandbox.write("a.txt", "old\n");

        let out = call(
            &WriteTool,
            &sandbox,
            json!({"path": "a.txt", "content": "new\n"}),
        )
        .unwrap();
        let payload: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["path"], "a.txt");
        assert_eq!(payload["bytes"], 4);
        assert!(payload["hash"].as_str().unwrap().len() == 64);
        assert_eq!(sandbox.read("a.txt"), "new\n");
    }

    #[test]
    fn creates_parent_directories() {
        let sandbox = Sandbox::new("write-mkdir");
        call(
            &WriteTool,
            &sandbox,
            json!({"path": "src/deep/a.txt", "content": "hi\n"}),
        )
        .unwrap();
        assert_eq!(sandbox.read("src/deep/a.txt"), "hi\n");
    }

    #[test]
    fn empty_content_is_allowed() {
        let sandbox = Sandbox::new("write-empty");
        sandbox.write("a.txt", "old\n");

        let out = call(
            &WriteTool,
            &sandbox,
            json!({"path": "a.txt", "content": ""}),
        )
        .unwrap();
        assert!(out.content.contains("\"bytes\":0"));
        assert_eq!(sandbox.read("a.txt"), "");
    }

    #[test]
    fn missing_content_is_bad_args() {
        let sandbox = Sandbox::new("write-nocontent");
        let err = call(&WriteTool, &sandbox, json!({"path": "a.txt"})).unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert!(!err.is_fatal());
    }

    #[test]
    fn stale_hash_is_rejected_without_touching_disk() {
        let sandbox = Sandbox::new("write-lock");
        sandbox.write("a.txt", "current\n");

        let err = call(
            &WriteTool,
            &sandbox,
            json!({"path": "a.txt", "content": "sneaky\n", "expected_hash": "deadbeef"}),
        )
        .unwrap_err();
        assert!(!err.is_fatal(), "乐观锁冲突应回灌给模型重新读取");
        assert_eq!(sandbox.read("a.txt"), "current\n");
    }

    #[test]
    fn matching_hash_is_accepted() {
        let sandbox = Sandbox::new("write-lock-ok");
        sandbox.write("a.txt", "current\n");
        let hash = fs_ops::hash_bytes(b"current\n");

        call(
            &WriteTool,
            &sandbox,
            json!({"path": "a.txt", "content": "next\n", "expected_hash": hash}),
        )
        .unwrap();
        assert_eq!(sandbox.read("a.txt"), "next\n");
    }

    #[test]
    fn sensitive_path_is_fatal_with_1003() {
        let sandbox = Sandbox::new("write-secret");
        let err = call(
            &WriteTool,
            &sandbox,
            json!({"path": ".env", "content": "SECRET=1"}),
        )
        .unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_DENIED);
        assert!(!sandbox.exists(".env"), "被拒的写入不能留下任何痕迹");
    }

    #[test]
    fn out_of_workspace_path_is_fatal_with_1002() {
        let sandbox = Sandbox::new("write-escape");
        let err = call(
            &WriteTool,
            &sandbox,
            json!({"path": "../outside.txt", "content": "x"}),
        )
        .unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    }
}
