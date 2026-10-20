//! `grep`：在工作区文件内容里检索。

use super::Args;
use crate::error::AgentError;
use crate::message::escape_xml;
use crate::registry::{Tool, ToolContext, ToolOutcome};
use ai_ide_indexer::grep;
use ai_ide_protocol::{SearchContentParams, SearchContentResult};
use serde_json::{json, Value};

const NAME: &str = "grep";

/// 内容检索。
///
/// 直接复用 M2 的 `search.content` 实现（`ai-ide-indexer`）：同一套 .gitignore 规则、
/// 同一套二进制/大文件跳过策略，避免「协议检索」与「工具检索」给出两套结果。
pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &'static str {
        "在工作区文件内容里检索，返回 路径:行:列 + 该行预览。默认按字面量、忽略大小写；可用 includes/excludes 限定 glob。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "查询串" },
                "is_regex": { "type": "boolean", "description": "是否按正则解释，默认 false" },
                "case_sensitive": { "type": "boolean", "description": "是否大小写敏感，默认 false" },
                "includes": { "type": "array", "items": { "type": "string" }, "description": "包含的 glob，例如 [\"src/**\"]" },
                "excludes": { "type": "array", "items": { "type": "string" }, "description": "排除的 glob" },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "最大命中数，缺省 500" }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let query = args.str("query")?;

        let params = SearchContentParams {
            workspace_id: ctx.workspace_id.to_string(),
            query: query.to_string(),
            is_regex: args.opt_bool("is_regex")?.unwrap_or(false),
            case_sensitive: args.opt_bool("case_sensitive")?.unwrap_or(false),
            includes: args.opt_str_list("includes")?,
            excludes: args.opt_str_list("excludes")?,
            max_results: args.opt_u32("max_results")?,
        };

        let result = grep::search(ctx.root, &params)?;
        Ok(ToolOutcome::new(render(query, &result)))
    }
}

fn render(query: &str, result: &SearchContentResult) -> String {
    let mut out = format!(
        "<grep query=\"{}\" matches=\"{}\"",
        escape_xml(query),
        result.matches.len()
    );
    if result.truncated {
        out.push_str(" truncated=\"true\"");
    }
    out.push_str(">\n");
    if result.matches.is_empty() {
        out.push_str("（无命中）\n");
    }
    for hit in &result.matches {
        out.push_str(&format!(
            "{}:{}:{}: {}\n",
            hit.path, hit.line, hit.column, hit.preview
        ));
    }
    out.push_str("</grep>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, Sandbox};

    #[test]
    fn finds_matches_with_position() {
        let sandbox = Sandbox::new("grep-basic");
        sandbox.write("src/a.ts", "const needle = 1;\nconst other = 2;\n");

        let out = call(&GrepTool, &sandbox, json!({"query": "needle"})).unwrap();
        assert!(out.content.contains("matches=\"1\""));
        assert!(out.content.contains("src/a.ts:1:7:"));
        assert!(!out.content.contains("truncated"));
    }

    #[test]
    fn no_hits_is_not_an_error() {
        let sandbox = Sandbox::new("grep-empty");
        sandbox.write("a.txt", "hello\n");

        let out = call(&GrepTool, &sandbox, json!({"query": "zzz"})).unwrap();
        assert!(out.content.contains("matches=\"0\""));
        assert!(out.content.contains("（无命中）"));
    }

    #[test]
    fn includes_glob_narrows_the_scope() {
        let sandbox = Sandbox::new("grep-includes");
        sandbox.write("src/a.txt", "needle\n");
        sandbox.write("docs/b.txt", "needle\n");

        let out = call(
            &GrepTool,
            &sandbox,
            json!({"query": "needle", "includes": ["src/**"]}),
        )
        .unwrap();
        assert!(out.content.contains("src/a.txt"));
        assert!(!out.content.contains("docs/b.txt"));
    }

    #[test]
    fn max_results_marks_truncation() {
        let sandbox = Sandbox::new("grep-truncated");
        let content: String = (0..10).map(|_| "needle\n").collect();
        sandbox.write("a.txt", &content);

        let out = call(
            &GrepTool,
            &sandbox,
            json!({"query": "needle", "max_results": 3}),
        )
        .unwrap();
        assert!(out.content.contains("matches=\"3\""));
        assert!(out.content.contains("truncated=\"true\""));
    }

    #[test]
    fn bad_regex_is_recoverable() {
        let sandbox = Sandbox::new("grep-badregex");
        sandbox.write("a.txt", "hello\n");

        let err = call(
            &GrepTool,
            &sandbox,
            json!({"query": "(unclosed", "is_regex": true}),
        )
        .unwrap_err();
        assert!(!err.is_fatal(), "坏正则应回灌给模型改");
    }

    #[test]
    fn empty_query_is_bad_args() {
        let sandbox = Sandbox::new("grep-noquery");
        let err = call(&GrepTool, &sandbox, json!({"query": "  "})).unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
    }
}
