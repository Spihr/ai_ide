//! `apply_diff`：unified diff 的生成与逐块落地。
//!
//! 一个工具两种姿势，按「给 `content` 还是给 `patch`」区分：
//!
//! - **生成**（给 `content`）：把「磁盘现状 → 新内容」算成 unified diff **返回**，
//!   不落盘。模型先看差异、人先审差异，再决定要不要贴。
//! - **应用**（给 `patch`）：把补丁贴到文件上。`accept` 可以只接受其中几块，
//!   其余整块跳过（新增行丢弃、被删的行原样留着）。
//!
//! 排版、解析与冲突判定全部交给 [`crate::diff`]：同一个引擎既服务这个工具，
//! 也服务后续的 diff 视图，避免「工具一套口径、界面另一套口径」。
//!
//! 三条边界：
//!
//! 1. **闸门先行**：越界 `1002`、敏感路径 `1003` 在写任何东西之前就被拒。
//! 2. **对不上就是冲突**：上下文行不匹配、hunk 之间重叠越界、`accept` 序号越界，
//!    一律 [`AgentError::Conflict`]（协议 `3003`），并且**一个字节都不写**——
//!    引擎边校验边拼装，出错时调用方拿不到半成品，所以不会留下被改坏的文件。
//! 3. **上限与 `read` 对齐**：单文件 256 KiB。超了 `read` 本身就看不全，
//!    让模型闭着眼睛改反而更危险。

use super::Args;
use crate::diff::{self, DiffError, DiffLineKind};
use crate::error::AgentError;
use crate::registry::{Tool, ToolContext, ToolOutcome};
use ai_ide_workspace::fs_ops;
use serde_json::{json, Value};

const NAME: &str = "apply_diff";

/// 单文件上限，与 `read` / `edit` 保持一致。
const MAX_BYTES: usize = 256 * 1024;

/// 两种姿势，二选一。
#[derive(Clone, Copy)]
enum Mode<'a> {
    /// 给 `content`：算差异、渲染成文本返回，不落盘。
    Generate(&'a str),
    /// 给 `patch`：把补丁贴到文件上。
    Apply(&'a str),
}

/// 生成 / 应用 unified diff。
pub struct ApplyDiffTool;

impl Tool for ApplyDiffTool {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &'static str {
        "生成或应用 unified diff（不经过 shell）。给 `content` 就把它与磁盘现状的差异渲染成 unified diff 返回，不落盘；给 `patch` 就把补丁按 hunk 贴到文件上，`accept` 可只接受其中几块。上下文对不上返回 3003 冲突，且不会写出半成品。单文件上限 256 KiB。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区根的路径，用 `/` 分隔" },
                "content": { "type": "string", "description": "生成模式：改后的完整内容（允许空串＝清空）；与 `patch` 二选一" },
                "patch": { "type": "string", "description": "应用模式：unified diff 文本；与 `content` 二选一" },
                "accept": {
                    "type": "array",
                    "items": { "type": "integer", "minimum": 0 },
                    "description": "应用模式：只接受这些 hunk 序号（0 起）；缺省＝全收，空数组＝一块都不收"
                },
                "expected_hash": { "type": "string", "description": "应用模式：磁盘当前的 SHA-256，给了就做乐观锁校验" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let args = Args::new(NAME, args);
        let path = args.str("path")?;
        let expected_hash = args.opt_str("expected_hash")?;
        let content = args.opt_text("content")?;
        let patch = args.opt_text("patch")?;
        // `accept: []` 意思是「一块都不接受」，跟「没给」不是一回事，所以要看它在不在。
        let explicit_accept = args.has("accept");
        let accept = args.opt_u32_list("accept")?;

        let mode = match (content, patch) {
            (Some(content), None) => Mode::Generate(content),
            (None, Some(patch)) => Mode::Apply(patch),
            (Some(_), Some(_)) => {
                return Err(bad_args("`content`（生成）与 `patch`（应用）只能给一个"))
            }
            (None, None) => {
                return Err(bad_args(
                    "要么给 `content` 生成 diff，要么给 `patch` 应用 diff",
                ))
            }
        };
        if explicit_accept && matches!(mode, Mode::Generate(_)) {
            return Err(bad_args("`accept` 只在应用模式（给了 `patch`）下有效"));
        }

        // 闸门先行：越界 1002、敏感路径 1003 都在这里被拒，且没写过任何东西。
        let abs = ctx.guard.resolve_for_write(path)?;
        let relative = ctx.display_path(&abs);
        let existed = abs.is_file();
        let (original, base_hash) = read_original(ctx, path, &relative, existed)?;

        match mode {
            Mode::Generate(content) => {
                generate(&relative, content, &original, existed, base_hash.as_deref())
            }
            Mode::Apply(patch) => apply(
                ctx,
                path,
                &relative,
                &original,
                patch,
                &accept,
                explicit_accept,
                expected_hash,
            ),
        }
    }
}

// ---------------------------------------------------------------- 读现状

/// 读文件现状，返回「规范化后的内容」与「磁盘完整字节的哈希」。
///
/// 文件不存在时按空串起算：纯新增的补丁因此也能贴（`@@ -0,0 +1,N @@`），
/// 而正文里带上下文的补丁会照常报冲突——不用担心把补丁贴错地方。
fn read_original(
    ctx: &ToolContext<'_>,
    path: &str,
    relative: &str,
    existed: bool,
) -> Result<(String, Option<String>), AgentError> {
    if !existed {
        return Ok((String::new(), None));
    }

    let file = fs_ops::read_file(ctx.guard, path, None, Some(MAX_BYTES as u64))?;
    if file.truncated {
        return Err(AgentError::ToolFailed {
            tool: NAME.to_string(),
            message: format!(
                "{relative} 超过 {MAX_BYTES} 字节，`read` 只能带回来前 {MAX_BYTES} 字节；改它等于盲改，请先把改动拆到小块文件上"
            ),
        });
    }
    Ok((file.content, Some(file.hash)))
}

// ---------------------------------------------------------------- 生成

/// 生成模式：算差异、渲染文本，**不碰磁盘**。
fn generate(
    relative: &str,
    content: &str,
    original: &str,
    existed: bool,
    base_hash: Option<&str>,
) -> Result<ToolOutcome, AgentError> {
    if content.len() > MAX_BYTES {
        return Err(too_big(relative, content.len()));
    }

    let hunks = diff::diff_hunks(original, content, diff::DEFAULT_CONTEXT);
    let added = count(&hunks, DiffLineKind::Added);
    let removed = count(&hunks, DiffLineKind::Removed);

    Ok(ToolOutcome::new(
        json!({
            "ok": true,
            "mode": "generate",
            "path": relative,
            "existed": existed,
            "changed": !hunks.is_empty(),
            "hunks": hunks.len(),
            "added": added,
            "removed": removed,
            // 生成不落盘，所以这里回的是「算差异时看到的」那份哈希。模型可以把它
            // 原样当 `expected_hash` 用，挡掉「生成」与「应用」之间被插一脚的情况。
            "base_hash": base_hash,
            "patch": diff::render_unified_diff(relative, &hunks),
        })
        .to_string(),
    ))
}

// ---------------------------------------------------------------- 应用

/// 应用模式：解析补丁、逐块落地。
#[allow(clippy::too_many_arguments)]
fn apply(
    ctx: &ToolContext<'_>,
    path: &str,
    relative: &str,
    original: &str,
    patch: &str,
    accept: &[u32],
    explicit_accept: bool,
    expected_hash: Option<&str>,
) -> Result<ToolOutcome, AgentError> {
    let parsed = diff::parse_unified_diff(patch).map_err(map_diff_error)?;
    if parsed.hunks.is_empty() {
        return Err(bad_args(
            "`patch` 里一个 hunk 都没有；请给一份真正的 unified diff，或改用 `content` 生成",
        ));
    }

    // 文件头只是参考，落点由 `path` 决定；但两者都给了却不一致就得拦下来，
    // 否则「补丁写着 A、参数写着 B」会静默改错文件。
    if let Some(declared) = parsed.path.as_deref() {
        if !declared.is_empty() && declared != relative {
            return Err(bad_args(&format!(
                "`patch` 的文件头指向 `{declared}`，而参数 `path` 是 `{relative}`；请让两者一致，或去掉文件头"
            )));
        }
    }

    let total = parsed.hunks.len();
    let (updated, accepted) = if explicit_accept {
        let indices: Vec<usize> = accept.iter().map(|index| *index as usize).collect();
        // 序号越界会以冲突（3003）返回，这里不必自己判一遍。
        let updated =
            diff::apply_selection(original, &parsed.hunks, &indices).map_err(map_diff_error)?;
        let mut flags = vec![false; total];
        for index in &indices {
            if let Some(slot) = flags.get_mut(*index) {
                *slot = true;
            }
        }
        (updated, flags)
    } else {
        let flags = vec![true; total];
        let updated = diff::apply_hunks(original, &parsed.hunks, &flags).map_err(map_diff_error)?;
        (updated, flags)
    };

    let changed = updated != original;
    if changed && updated.len() > MAX_BYTES {
        return Err(too_big(relative, updated.len()));
    }

    // 一块都没接受、或改动刚好抵消时，不写盘：没必要为了「什么都没变」去动文件。
    // 乐观锁交给 `write_file`，与 `edit` 一个口径。
    let hash = if changed {
        Some(fs_ops::write_file(ctx.guard, path, &updated, expected_hash, None)?.hash)
    } else {
        None
    };

    let accepted_indices: Vec<usize> = accepted
        .iter()
        .enumerate()
        .filter(|(_, ok)| **ok)
        .map(|(index, _)| index)
        .collect();
    let rejected_indices: Vec<usize> = accepted
        .iter()
        .enumerate()
        .filter(|(_, ok)| !**ok)
        .map(|(index, _)| index)
        .collect();

    Ok(ToolOutcome::new(
        json!({
            "ok": true,
            "mode": "apply",
            "path": relative,
            "changed": changed,
            "hunks": total,
            "accepted": accepted_indices,
            "rejected": rejected_indices,
            "bytes": updated.len(),
            "hash": hash,
        })
        .to_string(),
    ))
}

// ---------------------------------------------------------------- 小工具

/// 数一类行有多少。
fn count(hunks: &[diff::Hunk], kind: DiffLineKind) -> usize {
    hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .filter(|line| line.kind == kind)
        .count()
}

fn bad_args(reason: &str) -> AgentError {
    AgentError::BadArgs {
        tool: NAME.to_string(),
        reason: reason.to_string(),
    }
}

fn too_big(relative: &str, bytes: usize) -> AgentError {
    AgentError::ToolFailed {
        tool: NAME.to_string(),
        message: format!(
            "{relative} 会变成 {bytes} 字节，超过单文件上限 {MAX_BYTES} 字节；请把改动拆成多次小块"
        ),
    }
}

/// 引擎错误 → 协议错误：贴不上是 `3003` 冲突，补丁本身有毛病是参数错。
/// 两者都不致命（回灌给模型自己纠正），与 `edit` 的重试路径一致。
fn map_diff_error(err: DiffError) -> AgentError {
    match err {
        DiffError::Conflict(reason) => AgentError::Conflict {
            tool: NAME.to_string(),
            message: format!(
                "补丁与文件现状对不上：{reason}。请先 `read` 确认现状，再重新生成补丁"
            ),
        },
        DiffError::Parse(reason) => bad_args(&format!("`patch` 不是合法的 unified diff：{reason}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, Sandbox};
    use ai_ide_protocol::ErrorCode;

    /// 工具返回的 JSON 正文。
    fn result_of(outcome: &ToolOutcome) -> Value {
        serde_json::from_str(&outcome.content).expect("工具返回应是 JSON")
    }

    fn generate_patch(sandbox: &Sandbox, path: &str, content: &str) -> Value {
        result_of(
            &call(
                &ApplyDiffTool,
                sandbox,
                json!({ "path": path, "content": content }),
            )
            .unwrap(),
        )
    }

    #[test]
    fn generate_then_apply_round_trip() {
        let sandbox = Sandbox::new("diff-roundtrip");
        sandbox.write("src/app.ts", "let a = 1;\nlet b = 2;\nlet c = 3;\n");

        let generated = generate_patch(
            &sandbox,
            "src/app.ts",
            "let a = 1;\nlet b = 22;\nlet c = 3;\nlet d = 4;\n",
        );
        assert_eq!(generated["ok"], true);
        assert_eq!(generated["mode"], "generate");
        assert_eq!(generated["path"], "src/app.ts");
        assert_eq!(generated["existed"], true);
        assert_eq!(generated["changed"], true);
        assert_eq!(generated["hunks"], 1);
        assert_eq!(generated["added"], 2);
        assert_eq!(generated["removed"], 1);
        assert!(generated["base_hash"].is_string(), "生成模式要回磁盘哈希");

        // 生成阶段绝不落盘。
        assert_eq!(
            sandbox.read("src/app.ts"),
            "let a = 1;\nlet b = 2;\nlet c = 3;\n"
        );

        let patch = generated["patch"].as_str().unwrap().to_string();
        let applied = result_of(
            &call(
                &ApplyDiffTool,
                &sandbox,
                json!({ "path": "src/app.ts", "patch": patch }),
            )
            .unwrap(),
        );
        assert_eq!(applied["mode"], "apply");
        assert_eq!(applied["changed"], true);
        assert_eq!(applied["hunks"], 1);
        assert_eq!(applied["accepted"], json!([0]));
        assert_eq!(applied["rejected"], json!([]));
        assert!(applied["hash"].is_string(), "真的写了盘才回哈希");
        assert_eq!(
            sandbox.read("src/app.ts"),
            "let a = 1;\nlet b = 22;\nlet c = 3;\nlet d = 4;\n"
        );
    }

    #[test]
    fn generated_patch_is_deterministic_git_style_text() {
        let sandbox = Sandbox::new("diff-text");
        // 8 行里只动第 4 行：上下文取 3 上 3 下，第 8 行落在窗口外。
        sandbox.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n");

        let first = generate_patch(&sandbox, "a.txt", "1\n2\n3\nX\n5\n6\n7\n8\n");
        assert_eq!(
            first["patch"].as_str().unwrap(),
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,7 +1,7 @@\n 1\n 2\n 3\n-4\n+X\n 5\n 6\n 7\n"
        );

        // 同一份输入必须渲染成同一串字节（diff 视图与工具共用这份输出）。
        let second = generate_patch(&sandbox, "a.txt", "1\n2\n3\nX\n5\n6\n7\n8\n");
        assert_eq!(first["patch"], second["patch"]);
    }

    #[test]
    fn identical_content_produces_an_empty_patch() {
        let sandbox = Sandbox::new("diff-noop");
        sandbox.write("a.txt", "same\n");
        let generated = generate_patch(&sandbox, "a.txt", "same\n");
        assert_eq!(generated["changed"], false);
        assert_eq!(generated["hunks"], 0);
        assert_eq!(generated["patch"], "");
        assert_eq!(generated["added"], 0);
        assert_eq!(generated["removed"], 0);
    }

    #[test]
    fn a_subset_of_hunks_can_be_accepted() {
        let sandbox = Sandbox::new("diff-subset");
        let original: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        sandbox.write("many.txt", &original);
        let updated = original
            .replace("line 2\n", "line 2 changed\n")
            .replace("line 18\n", "line 18 changed\n");

        let generated = generate_patch(&sandbox, "many.txt", &updated);
        assert_eq!(generated["hunks"], 2, "两处改动隔着 3 行以上，应切成两块");
        let patch = generated["patch"].as_str().unwrap().to_string();

        // 只接受第 2 块：第 1 块的新增行必须原样丢弃、被删的行必须留着。
        let applied = result_of(
            &call(
                &ApplyDiffTool,
                &sandbox,
                json!({ "path": "many.txt", "patch": patch, "accept": [1] }),
            )
            .unwrap(),
        );
        assert_eq!(applied["changed"], true);
        assert_eq!(applied["hunks"], 2);
        assert_eq!(applied["accepted"], json!([1]));
        assert_eq!(applied["rejected"], json!([0]));

        let expected = original.replace("line 18\n", "line 18 changed\n");
        assert_eq!(sandbox.read("many.txt"), expected);
        assert!(!sandbox.read("many.txt").contains("line 2 changed"));
    }

    #[test]
    fn an_empty_accept_list_rejects_every_hunk() {
        let sandbox = Sandbox::new("diff-reject-all");
        sandbox.write("a.txt", "alpha\nbeta\ngamma\n");
        let patch = generate_patch(&sandbox, "a.txt", "alpha\nBETA\ngamma\n")["patch"]
            .as_str()
            .unwrap()
            .to_string();

        let applied = result_of(
            &call(
                &ApplyDiffTool,
                &sandbox,
                json!({ "path": "a.txt", "patch": patch, "accept": [] }),
            )
            .unwrap(),
        );
        assert_eq!(applied["changed"], false);
        assert_eq!(applied["accepted"], json!([]));
        assert_eq!(applied["rejected"], json!([0]));
        assert!(applied["hash"].is_null(), "没写盘就不该有哈希");
        assert_eq!(sandbox.read("a.txt"), "alpha\nbeta\ngamma\n");
    }

    #[test]
    fn a_stale_patch_conflicts_with_3003_and_writes_nothing() {
        let sandbox = Sandbox::new("diff-stale");
        sandbox.write("a.txt", "alpha\nbeta\ngamma\n");
        let patch = generate_patch(&sandbox, "a.txt", "alpha\nBETA\ngamma\n")["patch"]
            .as_str()
            .unwrap()
            .to_string();

        // 补丁生成之后，文件被别人改了。
        sandbox.write("a.txt", "alpha\nbeta 被别人改过\ngamma\n");

        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "a.txt", "patch": patch }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::DIFF_APPLY_CONFLICT, "复用 3003");
        assert_eq!(err.code().as_i32(), 3003);
        assert!(!err.is_fatal(), "冲突要回灌给模型，而不是终止整条对话");
        assert_eq!(
            sandbox.read("a.txt"),
            "alpha\nbeta 被别人改过\ngamma\n",
            "冲突时一个字节都不许写"
        );
    }

    #[test]
    fn an_out_of_range_accept_index_is_a_conflict() {
        let sandbox = Sandbox::new("diff-bad-index");
        sandbox.write("a.txt", "alpha\nbeta\ngamma\n");
        let patch = generate_patch(&sandbox, "a.txt", "alpha\nBETA\ngamma\n")["patch"]
            .as_str()
            .unwrap()
            .to_string();

        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "a.txt", "patch": patch, "accept": [7] }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::DIFF_APPLY_CONFLICT);
        assert_eq!(sandbox.read("a.txt"), "alpha\nbeta\ngamma\n");
    }

    #[test]
    fn a_broken_or_empty_patch_is_a_recoverable_bad_args() {
        let sandbox = Sandbox::new("diff-broken");

        // 完全没有 hunk：不是一份可用的补丁。
        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "a.txt", "patch": "这不是补丁" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::INVALID_PARAMS);
        assert!(!err.is_fatal(), "让模型重发一版就行");

        // hunk 正文里出现无法识别的行前缀：解析当场失败。
        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "a.txt", "patch": "@@ -1,2 +1,2 @@\n alpha\n!beta\n" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::INVALID_PARAMS);
        assert!(!sandbox.exists("a.txt"), "坏补丁不该顺手建出文件");
    }

    #[test]
    fn a_patch_can_create_a_missing_file() {
        let sandbox = Sandbox::new("diff-create");
        let generated = generate_patch(&sandbox, "new/dir/note.md", "hello\nworld\n");
        assert_eq!(generated["existed"], false);
        assert!(
            generated["base_hash"].is_null(),
            "文件还不存在，没有现状哈希"
        );
        assert_eq!(generated["added"], 2);
        assert_eq!(generated["removed"], 0);
        let patch = generated["patch"].as_str().unwrap().to_string();
        assert!(
            patch.contains("@@ -0,0 +1,2 @@"),
            "新文件按 GNU 约定从 0 行起步：{patch}"
        );
        assert!(!sandbox.exists("new/dir/note.md"), "生成阶段不该建文件");

        let applied = result_of(
            &call(
                &ApplyDiffTool,
                &sandbox,
                json!({ "path": "new/dir/note.md", "patch": patch }),
            )
            .unwrap(),
        );
        assert_eq!(applied["changed"], true);
        assert_eq!(sandbox.read("new/dir/note.md"), "hello\nworld\n");
    }

    #[test]
    fn a_patch_for_another_file_is_refused() {
        let sandbox = Sandbox::new("diff-mismatch");
        sandbox.write("a.txt", "alpha\n");
        sandbox.write("b.txt", "alpha\n");

        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "b.txt", "patch": "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-alpha\n+ALPHA\n" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::INVALID_PARAMS);
        assert_eq!(sandbox.read("b.txt"), "alpha\n", "改错文件比报错严重得多");
    }

    #[test]
    fn expected_hash_locks_the_apply() {
        let sandbox = Sandbox::new("diff-lock");
        sandbox.write("a.txt", "alpha\nbeta\ngamma\n");
        let generated = generate_patch(&sandbox, "a.txt", "alpha\nBETA\ngamma\n");
        let patch = generated["patch"].as_str().unwrap().to_string();
        let base_hash = generated["base_hash"].as_str().unwrap().to_string();

        // 有人抢在前面改了文件：老哈希用不上，写不进去。
        sandbox.write("a.txt", "alpha\nbeta2\ngamma\n");
        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "a.txt", "patch": patch.clone(), "expected_hash": base_hash }),
        )
        .unwrap_err();
        assert_eq!(
            err.code(),
            ErrorCode::DIFF_APPLY_CONFLICT,
            "乐观锁失败复用 3003"
        );
        assert_eq!(sandbox.read("a.txt"), "alpha\nbeta2\ngamma\n");

        // 重新取现状哈希后就能贴上去。
        let regen = generate_patch(&sandbox, "a.txt", "alpha\nBETA\ngamma\n");
        let fresh = regen["base_hash"].as_str().unwrap().to_string();
        let applied = result_of(
            &call(
                &ApplyDiffTool,
                &sandbox,
                json!({
                    "path": "a.txt",
                    "patch": regen["patch"].as_str().unwrap(),
                    "expected_hash": fresh
                }),
            )
            .unwrap(),
        );
        assert_eq!(applied["changed"], true);
        assert_eq!(sandbox.read("a.txt"), "alpha\nBETA\ngamma\n");
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let sandbox = Sandbox::new("diff-crlf");
        sandbox.write("win.txt", "a\r\nb\r\nc\r\n");

        let generated = generate_patch(&sandbox, "win.txt", "a\nB\nc\n");
        let patch = generated["patch"].as_str().unwrap().to_string();
        // 补丁本身是纯 LF 的：同一份改动在任何平台上都是同一串字节。
        assert!(!patch.contains('\r'), "补丁不该带 CR：{patch:?}");
        assert_eq!(
            patch,
            "--- a/win.txt\n+++ b/win.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
        );

        call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "win.txt", "patch": patch }),
        )
        .unwrap();
        let raw = sandbox.read("win.txt");
        assert_eq!(raw, "a\r\nB\r\nc\r\n", "落盘要沿用原文件的 CRLF");
        assert!(
            !raw.replace("\r\n", "").contains('\n'),
            "不该混进裸 LF：{raw:?}"
        );
    }

    #[test]
    fn both_modes_or_neither_is_a_bad_args() {
        let sandbox = Sandbox::new("diff-mode");
        for args in [
            json!({ "path": "a.txt" }),
            json!({ "path": "a.txt", "content": "x\n", "patch": "--- a/a.txt\n" }),
            json!({ "path": "a.txt", "content": "x\n", "accept": [0] }),
        ] {
            let err = call(&ApplyDiffTool, &sandbox, args.clone()).unwrap_err();
            assert_eq!(err.code(), ErrorCode::INVALID_PARAMS, "{args}");
        }
    }

    #[test]
    fn oversized_target_is_refused_before_writing() {
        let sandbox = Sandbox::new("diff-big");
        sandbox.write("a.txt", "small\n");

        let huge = "x".repeat(MAX_BYTES + 1);
        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "a.txt", "content": huge.clone() }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::INTERNAL_ERROR);
        assert_eq!(sandbox.read("a.txt"), "small\n");

        // 现状本身超过上限时也不动它：read 看不全的文件不该被盲改。
        let mut big = "y".repeat(MAX_BYTES + 1);
        big.push('\n');
        sandbox.write("big.txt", &big);
        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "big.txt", "content": "y\n" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::INTERNAL_ERROR);
        assert_eq!(sandbox.read("big.txt"), big);
    }

    #[test]
    fn escaping_paths_and_sensitive_targets_are_refused() {
        let sandbox = Sandbox::new("diff-guard");

        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": "../outside.txt", "content": "nope\n" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
        assert!(err.is_fatal(), "越界是致命的，不该回灌让模型重试");

        let err = call(
            &ApplyDiffTool,
            &sandbox,
            json!({ "path": ".env", "patch": "--- a/.env\n+++ b/.env\n@@ -1,1 +1,1 @@\n-A=1\n+A=2\n" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::PATH_DENIED, "密钥文件连补丁都不许贴");
        assert!(err.is_fatal());
        assert!(!sandbox.exists("outside.txt"));
    }
}
