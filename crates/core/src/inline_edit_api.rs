//! `inline_edit.propose` 的处理器（M3 切片三）。
//!
//! 一条链路：**读选区与上下文 → 组 prompt → 复用 llm 运行时补全一次 → 提取代码 → 算 diff**。
//!
//! ## 三条刻意的取舍
//!
//! 1. **不写盘**。返回的是「应该改成什么」，落盘留给外壳走 `workspace.write_file`
//!    （带 `expected_hash` 乐观锁）。用户按 Esc 放弃时不需要任何撤销逻辑，也不会
//!    出现「内核改了文件、编辑器还开着旧内容」的撕裂态。
//! 2. **不改 `llm.chat_stream` 的外部行为**。这里另写一个 `complete_once`：
//!    流的开始/结束事件、`stream_id` 登记、取消表都是对话面的语义，编辑器里的一次
//!    改写不该出现在会话里，也不该占用 `stream_id`。
//! 3. **失败一律报码，绝不降级成空替换**。空替换落盘等于悄悄删掉用户选中的代码；
//!    「模型只回了一段解释、没给代码」这种情况必须让用户看见 9003，而不是看到
//!    代码凭空消失。
//!
//! 选区口径见 [`TextSelection`]：行号 1 起算闭区间，列号 0 起算右开（与 LSP 一致）。

use std::sync::Arc;

use ai_ide_llm::{CancelToken, ChatRequest, LlmError, Provider, StreamEvent};
use ai_ide_protocol::dto::{
    ChatMessage, ChatParams, ChatRole, InlineEditProposeParams, InlineEditProposeResult,
    InlineEditUsage, ModelRef, TextSelection,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_workspace::fs_ops;
use tokio::sync::mpsc::unbounded_channel;
use tracing::{debug, info, warn};

use crate::dispatch::parse_params;
use crate::llm_api::workspace_error;
use crate::peer::Peer;
use crate::state::AppState;

/// 缺省上下文行数：选区上下各 40 行。够模型认出「这是哪个函数、用了什么命名」，
/// 又不至于让一次改写吃掉几千 token。
const DEFAULT_CONTEXT_LINES: u32 = 40;

/// 上下文行数上限：再多也塞不进合理预算，而且超长前缀会让模型对选区定位变糊。
const MAX_CONTEXT_LINES: u32 = 400;

/// 指令长度上限：超过这个长度基本是用户误粘了整篇需求文档，不是「Inline Edit」。
const MAX_INSTRUCTION_CHARS: usize = 4000;

// ---------------------------------------------------------------------------
// 处理器
// ---------------------------------------------------------------------------

pub async fn propose(peer: &Peer, state: &AppState, req: &Request) {
    let params: InlineEditProposeParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    if let Some(err) = validate_params(&params) {
        return peer.respond_error(&req.id, &err).await;
    }

    // 1) 工作区：路径解析的根。用 `Some(..)` 而不是活动工作区——一次改写必须打在
    //    用户看得见的那份文件上，跟着「活动标签页」漂会打错文件。
    let workspace = match state.workspace(Some(&params.workspace_id)) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &workspace_error(&err)).await,
    };

    // 2) 模型：先把「能不能发」判掉。等读完文件再发现没模型，等于白读一次盘。
    let model_ref = match resolve_model(state, params.model_ref.as_ref()) {
        Ok(model_ref) => model_ref,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let provider = match state.llm().registry().get(&model_ref.provider_id) {
        Some(provider) => provider,
        None => {
            let err = LlmError::ProviderNotConfigured {
                provider_id: model_ref.provider_id.clone(),
            };
            return peer.respond_error(&req.id, &err.to_error_object()).await;
        }
    };

    // 3) 读整份文件：选区是按行/列算的，只有拿到全文才能切出正确的片段。
    //    读盘是阻塞活，扔进阻塞池。
    let path = params.path.clone();
    let document = {
        let guard = workspace.guard.clone();
        let path = path.clone();
        match tokio::task::spawn_blocking(move || fs_ops::read_file(&guard, &path, None, None))
            .await
        {
            Ok(Ok(read)) => read,
            Ok(Err(err)) => return peer.respond_error(&req.id, &workspace_error(&err)).await,
            Err(join_err) => {
                let err = ErrorObject::internal(format!("读取目标文件的任务异常退出：{join_err}"));
                return peer.respond_error(&req.id, &err).await;
            }
        }
    };

    // 截断的文件不能算补丁：行号还停在 8 MiB 处，后面全是空白，diff 会变成
    // 「把用户没看见的代码全删了」。这种情况只报错，不给半份结果。
    if document.truncated {
        let err = ErrorObject::new(
            ErrorCode::INLINE_EDIT_FILE_TOO_LARGE,
            format!(
                "目标文件超过读取上限（{} 字节），无法安全计算选区补丁",
                document.size
            ),
        )
        .with_data(serde_json::json!({ "path": params.path, "size": document.size }));
        return peer.respond_error(&req.id, &err).await;
    }

    // 4) 切选区。越界在这里一次性判死（9001），不做「体贴的收拢」。
    let lines: Vec<&str> = document.content.split('\n').collect();
    let selection = match Selection::resolve(&lines, &params.selection) {
        Ok(selection) => selection,
        Err(message) => {
            warn!(path = %params.path, %message, "选区无法解析");
            let err = ErrorObject::new(ErrorCode::INLINE_EDIT_SELECTION_INVALID, message)
                .with_data(serde_json::json!({
                    "path": params.path,
                    "lines": lines.len(),
                    "selection": params.selection,
                }));
            return peer.respond_error(&req.id, &err).await;
        }
    };

    // 5) 组 prompt。上下文按行切片带进去，行号标在每行行首——模型看得到「这是第几行」，
    //    才不会把缩进或相邻语句改着改着搬走。
    let context_lines = params
        .context_lines
        .unwrap_or(DEFAULT_CONTEXT_LINES)
        .min(MAX_CONTEXT_LINES);
    let prompt = build_prompt(
        &params,
        &lines,
        &selection,
        context_lines,
        language_of(&params.path),
    );

    // 6) 补全一次。走 `ai_ide_llm` 的 provider，与对话面同一套密钥、超时与协议适配。
    let request = ChatRequest {
        model: model_ref.clone(),
        messages: vec![
            ChatMessage {
                role: ChatRole::System,
                content: SYSTEM_PROMPT.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            ChatMessage {
                role: ChatRole::User,
                content: prompt,
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ],
        tools: Vec::new(),
        tool_choice: None,
        params: ChatParams {
            // 改写要「照指令改」，不是「照风格发挥」：温度压低，减少无关重排。
            temperature: Some(0.2),
            ..ChatParams::default()
        },
    };

    let (response, usage) = match complete_once(provider, request).await {
        Ok(done) => done,
        Err(err) => {
            warn!(
                path = %params.path,
                code = err.code().as_i32(),
                message = %err,
                "Inline Edit 上游失败"
            );
            return peer.respond_error(&req.id, &err.to_error_object()).await;
        }
    };

    // 7) 提取代码。提不出来就报 9003——绝不把整段解释当成代码，也绝不返回空替换。
    let replacement = match extract_code(&response) {
        Some(code) => code,
        None => {
            warn!(path = %params.path, chars = response.chars().count(), "模型响应中不含可用代码");
            let err = ErrorObject::new(
                ErrorCode::INLINE_EDIT_EMPTY_REPLACEMENT,
                "模型响应中提取不出代码（可能是空响应或只有解释），未生成替换内容",
            )
            .with_data(serde_json::json!({
                "path": params.path,
                "response_chars": response.chars().count(),
            }));
            return peer.respond_error(&req.id, &err).await;
        }
    };

    // 8) 算 diff。基于**整份文件**算，而不是只对选区算：行号要对得上编辑器里的
    //    真实行号，否则 diff 视图会把补丁挂在错误的位置。
    let updated = selection.splice(&document.content, &replacement);
    let diff = ai_ide_agent::diff::unified_diff(&params.path, &document.content, &updated, 3);

    info!(
        path = %params.path,
        provider_id = %model_ref.provider_id,
        model_id = %model_ref.model_id,
        original_lines = selection.line_span(),
        replacement_chars = replacement.chars().count(),
        diff_bytes = diff.len(),
        "Inline Edit 已生成替换"
    );

    let result = InlineEditProposeResult {
        path: params.path.clone(),
        range: params.selection,
        original: selection.text.clone(),
        replacement,
        diff,
        usage: usage.map(|usage| InlineEditUsage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cached_tokens: usage.cached_tokens,
        }),
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// 参数与模型解析
// ---------------------------------------------------------------------------

/// 报文级校验：只判「调用方写错」，不判「选区画错」——后者要带行数信息回给外壳，
/// 走 9001，两者混用会让「改一个参数再试」和「重新框选」分不清。
fn validate_params(params: &InlineEditProposeParams) -> Option<ErrorObject> {
    if params.path.trim().is_empty() {
        return Some(ErrorObject::invalid_params("path 不能为空"));
    }
    if params.instruction.trim().is_empty() {
        return Some(ErrorObject::invalid_params("instruction 不能为空"));
    }
    if params.instruction.chars().count() > MAX_INSTRUCTION_CHARS {
        return Some(ErrorObject::invalid_params(format!(
            "instruction 过长（{} 字符，上限 {MAX_INSTRUCTION_CHARS}）",
            params.instruction.chars().count()
        )));
    }
    if params.context_lines.is_some_and(|n| n > MAX_CONTEXT_LINES) {
        return Some(ErrorObject::invalid_params(format!(
            "context_lines 超出上限（{MAX_CONTEXT_LINES} 行）"
        )));
    }
    None
}

/// 定位这次改写走哪个模型。
///
/// 优先级：显式 `model_ref` → 目录里第一个可用模型。两条路径都要能回答
/// 「这个 provider 真的在注册表里、这个 model 真的在目录里」，否则就会把
/// 一次注定 401 / 404 的请求发出去，用户看到的却是「代码没变」。
fn resolve_model(state: &AppState, requested: Option<&ModelRef>) -> Result<ModelRef, ErrorObject> {
    let store = state.llm().store();
    let snapshot = store.load().map_err(|err| err.to_error_object())?;
    let registry = state.llm().registry();

    if let Some(model_ref) = requested {
        if model_ref.provider_id.trim().is_empty() {
            return Err(model_unavailable("model_ref.provider_id 不能为空"));
        }
        if model_ref.model_id.trim().is_empty() {
            return Err(model_unavailable("model_ref.model_id 不能为空"));
        }
        let pk = ai_ide_llm::model_pk(&model_ref.provider_id, &model_ref.model_id);
        let known = snapshot
            .models
            .iter()
            .any(|model| model.model_pk == pk && registry.get(&model.provider_id).is_some());
        if !known {
            return Err(
                model_unavailable(format!("模型不在目录中或所属 provider 未启用：{pk}"))
                    .with_data(serde_json::json!({ "model_pk": pk })),
            );
        }
        return Ok(model_ref.clone());
    }

    // 没指定就走目录里的第一个可用模型：与对话面的「默认模型」同一口径，
    // 免得用户在设置里选过的默认模型只对聊天生效、对 Inline Edit 不生效。
    let fallback = snapshot
        .models
        .iter()
        .find(|model| registry.get(&model.provider_id).is_some());
    match fallback {
        Some(model) => Ok(ModelRef {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
        }),
        None => {
            debug!("模型目录为空，Inline Edit 无法选模型");
            Err(model_unavailable(
                "没有可用模型：请先在设置里添加 provider 与模型",
            ))
        }
    }
}

fn model_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(ErrorCode::INLINE_EDIT_MODEL_UNAVAILABLE, message)
}

// ---------------------------------------------------------------------------
// 选区
// ---------------------------------------------------------------------------

/// 一处已解析的选区：行区间、两端列偏移、原文。
///
/// `Debug` 只为单测里 `expect_err` 能打印出「本来拿到了什么」，不参与对外契约。
#[derive(Debug)]
struct Selection {
    /// 起始行（1 起算）。
    start_line: u32,
    /// 结束行（1 起算，闭）。
    end_line: u32,
    /// 起始列（0 起算，含）。
    start_column: usize,
    /// 结束列（0 起算，不含）。
    end_column: usize,
    /// 选区原文。跨行时是拼接出来的，所以是 `String` 而不是 `&str`。
    text: String,
}

impl Selection {
    /// 校验并切出选区原文。
    ///
    /// 列偏移按 **Unicode 标量（char）** 计，不按字节：按字节切会在中文/emoji 处
    /// 切出半个字符，`&str` 直接 panic——那不是「错误码」，那是把内核搞崩。
    fn resolve(lines: &[&str], sel: &TextSelection) -> Result<Self, String> {
        let total = lines.len();
        if sel.start_line == 0 || sel.end_line == 0 {
            return Err("选区行号从 1 起算，不能为 0".to_string());
        }
        if sel.start_line as usize > total || sel.end_line as usize > total {
            return Err(format!(
                "选区越界：起始行 {}、结束行 {}，文件只有 {total} 行",
                sel.start_line, sel.end_line
            ));
        }
        if (sel.start_line, sel.start_column) > (sel.end_line, sel.end_column) {
            return Err(format!(
                "选区方向非法：起点 {}:{} 在终点 {}:{} 之后",
                sel.start_line, sel.start_column, sel.end_line, sel.end_column
            ));
        }

        let start_line_text = lines[sel.start_line as usize - 1];
        let end_line_text = lines[sel.end_line as usize - 1];
        let start_len = start_line_text.chars().count();
        let end_len = end_line_text.chars().count();
        let start_column = sel.start_column as usize;
        let end_column = sel.end_column as usize;

        // 列可以等于行长（= 行尾），但不能超过：超了就是「把光标放在不存在的字符上」。
        if start_column > start_len {
            return Err(format!(
                "选区越界：起始列 {start_column} 超过第 {} 行的长度 {start_len}",
                sel.start_line
            ));
        }
        if end_column > end_len {
            return Err(format!(
                "选区越界：结束列 {end_column} 超过第 {} 行的长度 {end_len}",
                sel.end_line
            ));
        }

        Ok(Self {
            start_line: sel.start_line,
            end_line: sel.end_line,
            start_column,
            end_column,
            text: extract_text(
                lines,
                sel.start_line,
                sel.end_line,
                start_column,
                end_column,
            ),
        })
    }

    /// 选区跨的行数（用于日志）。
    fn line_span(&self) -> u32 {
        self.end_line - self.start_line + 1
    }

    /// 把替换文本拼回整份文档：只换选区覆盖的那段，其余字节原样保留。
    fn splice(&self, content: &str, replacement: &str) -> String {
        // 按行号换算成字节偏移：先数前 `start_line - 1` 行的长度（含换行），
        // 再加起始列在这一行里的字节宽度。
        let mut offset = 0usize;
        let lines: Vec<&str> = content.split('\n').collect();
        for line in lines.iter().take(self.start_line as usize - 1) {
            offset += line.len() + 1; // `+ 1` 是被 split 吃掉的 '\n'
        }
        let start_bytes =
            offset + char_to_byte(lines[self.start_line as usize - 1], self.start_column);

        let mut end_offset = 0usize;
        for line in lines.iter().take(self.end_line as usize - 1) {
            end_offset += line.len() + 1;
        }
        let end_bytes =
            end_offset + char_to_byte(lines[self.end_line as usize - 1], self.end_column);

        let mut out = String::with_capacity(content.len() + replacement.len());
        out.push_str(&content[..start_bytes]);
        out.push_str(replacement);
        out.push_str(&content[end_bytes..]);
        out
    }
}

/// 第 `column` 个字符在该行里的字节偏移。
fn char_to_byte(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(byte, _)| byte)
}

/// 切出选区原文。单行零拷贝即可，跨行必须拼接（多行之间隔着被 `split` 吃掉的 `\n`）。
fn extract_text(
    lines: &[&str],
    start_line: u32,
    end_line: u32,
    start_column: usize,
    end_column: usize,
) -> String {
    let first = lines[start_line as usize - 1];
    let last = lines[end_line as usize - 1];
    let head = &first[char_to_byte(first, start_column)..];

    if start_line == end_line {
        let tail_byte = char_to_byte(last, end_column);
        return first[char_to_byte(first, start_column)..tail_byte].to_string();
    }

    let mut text = String::from(head);
    for line_no in (start_line + 1)..end_line {
        text.push('\n');
        text.push_str(lines[line_no as usize - 1]);
    }
    text.push('\n');
    text.push_str(&last[..char_to_byte(last, end_column)]);
    text
}

// ---------------------------------------------------------------------------
// 一次性补全（不改 `llm.chat_stream` 的外部行为）
// ---------------------------------------------------------------------------

/// 发一次请求、收完整段文本。
///
/// 与 `llm.chat_stream` 的区别只有三处，且都在「对外行为」之外：
/// 不推 `llm.*` 事件、不登记 `stream_id`、不需要对端句柄。
/// provider 那边仍然走同一条 `chat_stream`（它是唯一入口），因此密钥、超时、
/// 协议适配、错误折返完全复用——这里不该有第二套 HTTP 逻辑。
async fn complete_once(
    provider: Arc<dyn Provider>,
    request: ChatRequest,
) -> Result<(String, Option<ai_ide_llm::Usage>), LlmError> {
    let (tx, mut rx) = unbounded_channel::<StreamEvent>();
    let cancel = CancelToken::new();
    let task = tokio::spawn(async move { provider.chat_stream(request, tx, cancel).await });

    let mut text = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            // 思维链与工具调用对改写没用：不拼进正文，免得把模型的自我独白当成代码。
            StreamEvent::Delta { text: delta } => text.push_str(&delta),
            StreamEvent::Finished { .. } | StreamEvent::Usage(_) => {}
            StreamEvent::Reasoning { .. } | StreamEvent::ToolCall { .. } => {}
        }
    }

    match task.await {
        Ok(Ok(usage)) => Ok((text, Some(usage))),
        Ok(Err(err)) => Err(err),
        Err(join_err) => Err(LlmError::Unavailable {
            message: format!("补全任务异常退出：{join_err}"),
            status: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// prompt 与响应解析（纯函数，便于单测）
// ---------------------------------------------------------------------------

const SYSTEM_PROMPT: &str = "你是代码编辑器里的就地改写引擎。\
用户会给你一段文件上下文、一处用 <<<SELECTION>>> 标出的选区，以及一条改写指令。\
你只需要输出**替换选区后的新代码**，要求：\n\
1. 只输出选区对应的新代码，不要输出选区以外的行；\n\
2. 不要解释、不要道歉、不要写「以下是修改后的代码」；\n\
3. 用 ``` 围栏包住代码，围栏后不要再写任何内容；\n\
4. 保持原有的缩进风格与命名习惯；\n\
5. 若指令与代码无关或无法完成，输出 ``` 空围栏，不要编造无关代码。";

/// 组 prompt：上下文 + 带行号的选区 + 指令。
///
/// 行号标在每行行首，模型才看得见「这是第几行、选区从哪开始」；只给裸文本时，
/// 模型很容易把相邻行一起重排，然后「替换」多改了几行。
fn build_prompt(
    params: &InlineEditProposeParams,
    lines: &[&str],
    selection: &Selection,
    context_lines: u32,
    language: &str,
) -> String {
    let total = lines.len() as u32;
    let from = selection.start_line.saturating_sub(context_lines).max(1);
    let to = selection.end_line.saturating_add(context_lines).min(total);

    let mut body = String::new();
    for line_no in from..=to {
        let text = lines[line_no as usize - 1];
        if line_no == selection.start_line {
            // 选区起点：打一个显式标记，比靠行号对齐更抗模型「数错行」。
            body.push_str("<<<SELECTION>>>\n");
        }
        body.push_str(&format!("{line_no:>6} | {text}\n"));
        if line_no == selection.end_line {
            body.push_str("<<<END SELECTION>>>\n");
        }
    }

    format!(
        "文件：{path}\n语言：{language}\n选区：第 {start} 行第 {sc} 列 到 第 {end} 行第 {ec} 列\
（行号 1 起算；列号 0 起算、右开）\n选区原文：\n---\n{selected}\n---\n\n\
上下文（行号 | 内容）：\n{body}\n指令：{instruction}",
        path = params.path,
        language = language,
        start = selection.start_line,
        sc = selection.start_column,
        end = selection.end_line,
        ec = selection.end_column,
        selected = selection.text,
        body = body,
        instruction = params.instruction.trim(),
    )
}

/// 从模型响应里抠出代码。
///
/// 优先级：第一个围栏块 → 整段（去围栏行）。返回 `None` 表示「抠不出可用代码」，
/// 由调用方报 9003：这里**不做**「那就用整段吧」的兜底，因为一段解释被当成代码
/// 写进文件，比报错难查十倍。
fn extract_code(response: &str) -> Option<String> {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(code) = first_fence_body(trimmed) {
        let code = code.trim_end_matches(['\n', '\r']);
        if code.trim().is_empty() {
            // 模型明确回了「空围栏」：说明它认为无需改 / 无法改。当作失败报 9003，
            // 让用户自己决定是改指令还是取消，而不是静默把选区清空。
            return None;
        }
        return Some(code.to_string());
    }

    None
}

/// 取第一个围栏块的内容（` ```lang ` 到下一个 ` ``` `）。
fn first_fence_body(text: &str) -> Option<String> {
    let mut lines = text.lines();
    // 找开围栏：允许缩进（模型常把围栏缩进到代码块里）。
    let mut body = String::new();
    let mut inside = false;
    for line in lines.by_ref() {
        let candidate = line.trim_start();
        if !inside {
            if candidate.starts_with("```") {
                inside = true;
            }
            continue;
        }
        if candidate.starts_with("```") {
            return Some(body);
        }
        body.push_str(line);
        body.push('\n');
    }
    // 围栏没闭合：仍把已收集的内容当代码（模型偶尔漏掉收尾围栏），
    // 但空内容不算——空内容与「没给代码」等价。
    if inside && !body.trim().is_empty() {
        return Some(body);
    }
    None
}

/// 路径 → 语言标签。只用于 prompt 里的一句话提示，不做语法判断。
fn language_of(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cc" | "cpp" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" => "PowerShell",
        "sql" => "SQL",
        "html" | "htm" => "HTML",
        "css" | "scss" | "less" => "CSS",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "md" | "markdown" => "Markdown",
        _ => "纯文本",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(start_line: u32, start_column: u32, end_line: u32, end_column: u32) -> TextSelection {
        TextSelection {
            start_line,
            start_column,
            end_line,
            end_column,
        }
    }

    #[test]
    fn selection_slices_within_one_line() {
        let content = "let a = 1;\nlet b = 2;";
        let lines: Vec<&str> = content.split('\n').collect();
        let selection = Selection::resolve(&lines, &sel(2, 4, 2, 5)).expect("选区合法");
        assert_eq!(selection.text, "b");
        assert_eq!(selection.splice(content, "c"), "let a = 1;\nlet c = 2;");
    }

    #[test]
    fn selection_crossing_lines_keeps_boundaries() {
        let content = "a1\na2\na3";
        let lines: Vec<&str> = content.split('\n').collect();
        let selection = Selection::resolve(&lines, &sel(1, 1, 2, 1)).expect("选区合法");
        assert_eq!(selection.splice(content, "X"), "aX2\na3");
    }

    #[test]
    fn selection_out_of_range_is_rejected() {
        let content = "a\nb";
        let lines: Vec<&str> = content.split('\n').collect();
        let err = Selection::resolve(&lines, &sel(5, 0, 6, 0)).expect_err("越界必须报错");
        assert!(err.contains("越界"), "{err}");
    }

    #[test]
    fn selection_column_past_line_end_is_rejected() {
        let content = "abc";
        let lines: Vec<&str> = content.split('\n').collect();
        let err = Selection::resolve(&lines, &sel(1, 0, 1, 9)).expect_err("列越界必须报错");
        assert!(err.contains("超过"), "{err}");
    }

    #[test]
    fn selection_reversed_is_rejected() {
        let content = "a\nb\nc";
        let lines: Vec<&str> = content.split('\n').collect();
        let err = Selection::resolve(&lines, &sel(2, 0, 1, 0)).expect_err("方向反了必须报错");
        assert!(err.contains("方向非法"), "{err}");
    }

    #[test]
    fn multibyte_columns_do_not_split_chars() {
        // 按字节切会在这里切成半个汉字并 panic；按 char 切才是对的。
        let content = "中文变量 = 1;";
        let lines: Vec<&str> = content.split('\n').collect();
        let selection = Selection::resolve(&lines, &sel(1, 2, 1, 4)).expect("选区合法");
        assert_eq!(selection.text, "变量");
        // 列 0 起算右开：2..4 命中的是 `变量`，因此只剩 `中` + `值` + 尾部。
        assert_eq!(selection.splice(content, "值"), "中文值 = 1;");
    }

    #[test]
    fn extract_code_prefers_first_fence() {
        let response = "说明\n```rust\nlet x = 1;\n```\n尾部说明";
        assert_eq!(extract_code(response).as_deref(), Some("let x = 1;"));
    }

    #[test]
    fn extract_code_rejects_explanation_only() {
        // 只有解释没有围栏：宁可报错，也不能把这句话写进源码。
        let response = "这段代码看起来没问题，不需要修改。";
        assert!(extract_code(response).is_none());
    }

    #[test]
    fn extract_code_rejects_empty_fence() {
        let response = "```\n```";
        assert!(extract_code(response).is_none());
    }

    #[test]
    fn extract_code_accepts_unclosed_fence() {
        let response = "```ts\nconst a = 1;\n";
        assert_eq!(extract_code(response).as_deref(), Some("const a = 1;"));
    }

    #[test]
    fn language_label_covers_common_extensions() {
        assert_eq!(language_of("src/main.rs"), "Rust");
        assert_eq!(language_of("src/app.tsx"), "TypeScript");
        assert_eq!(language_of("README.md"), "Markdown");
        assert_eq!(language_of("Makefile"), "纯文本");
    }
}
