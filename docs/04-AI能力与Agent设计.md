---
AIGC:
    Label: "1"
    ContentProducer: 001191440300708461136T1XGW3
    ProduceID: 04526fdd866a8c5c9aceee6b83ed2c05_ee2f3675cad411f18e0e52540064ee0f
    ReservedCode1: RBMzJsJd2MXqaElFVYHaOKSOVQlQ6uZ2l9zAfwk/2vw48/vWru/CTJTS1WLTkzz2d01sCEkMSaXJwUc83kIZ46AGKL68+DeXYX7erYP+MLbAO02KLZDQkkvcL1J88yQN8N6+1Inoj74fm3KeTYhe4wwmZ9ALmVMXJnOjyuveMJXfaiZwPbpdQS6o5OE=
    ContentPropagator: 001191440300708461136T1XGW3
    PropagateID: 04526fdd866a8c5c9aceee6b83ed2c05_ee2f3675cad411f18e0e52540064ee0f
    ReservedCode2: RBMzJsJd2MXqaElFVYHaOKSOVQlQ6uZ2l9zAfwk/2vw48/vWru/CTJTS1WLTkzz2d01sCEkMSaXJwUc83kIZ46AGKL68+DeXYX7erYP+MLbAO02KLZDQkkvcL1J88yQN8N6+1Inoj74fm3KeTYhe4wwmZ9ALmVMXJnOjyuveMJXfaiZwPbpdQS6o5OE=
---

# AI 能力与 Agent 设计

> 文档版本：v0.1（待审阅）
> 依赖：`02-系统架构设计.md`、`03-内核接口协议规范.md`
> 实现状态：**模型接入（M2）、上下文组装与压缩（M3）、Agent 循环与权限审批（M4）已在内核与外壳两侧落地**；**检查点与 MCP 部分尚未实现**（对应 M5/M6/M7）。截至 2026-10-21，`crates/llm`（Provider 适配、SSE 流式解析、取消、错误分类、密钥加密、目录存储）、`crates/agent`（Agent 循环、工具注册表、权限闸门、diff）与 `crates/core/src/{llm_api,context,agent_api}.rs` 均已落地并接线；Provider 配置 UI、对话面板、`@` 引用与 Agent 运行/审批面板均已接入渲染端；`crates/mcp` / `crates/store` 尚未创建。本文其余内容（检查点、MCP 等）是 M5 起的施工图纸，不代表当前实现。

---

## 1. Provider 抽象层

### 1.1 抽象接口

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    /// 流式对话：内部把协议差异归一化为统一的 StreamEvent 序列
    async fn chat_stream(
        &self,
        req: ChatRequest,
        sink: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<Usage>;

    /// 模型发现（可选能力）
    async fn list_models(&self) -> Result<Vec<ModelInfo>> { Ok(vec![]) }

    /// 连通性测试
    async fn health(&self) -> Result<HealthReport>;

    fn capabilities(&self) -> ProviderCaps;
}

pub enum StreamEvent {
    Text(String),
    Reasoning(String),
    ToolCallDelta { index: usize, id: Option<String>, name: Option<String>, args_delta: String },
    Usage(Usage),
    Finish(FinishReason),
}
```

**核心思想**：所有协议差异在 Provider 实现内消化，上层 Agent 循环只面对 `ChatRequest` / `StreamEvent`。新增一家厂商 = 新增一个文件 + 在注册表登记，**不改动 Agent 与 UI**。

### 1.2 已规划实现

| 协议 | 端点 | 鉴权 | 工具调用 | 流式 |
|---|---|---|---|---|
| OpenAI Chat Completions | `POST {base}/chat/completions` | `Authorization: Bearer` | `tools` / `tool_calls` | SSE `data:` |
| OpenAI Responses | `POST {base}/responses` | 同上 | `tools` | SSE 事件类型 |
| Anthropic Messages | `POST {base}/v1/messages` | `x-api-key` + `anthropic-version` | `tools` / `tool_use` | SSE |
| Google Gemini | `POST {base}/v1beta/models/{m}:streamGenerateContent` | `?key=` 或 `x-goog-api-key` | `functionDeclarations` | SSE |
| Ollama | `POST {base}/api/chat` | 无 | `tools` | NDJSON 流 |
| 自定义 OpenAI 兼容 | 同 OpenAI | 可自定义头 | 取决于实现 | 同上 |

### 1.3 兼容性处理清单（易踩的坑）

1. **增量工具参数**：部分厂商按 index 拼接、部分带 id、部分只给完整 JSON；统一在 Provider 内做累加器，输出完整 `ToolCall`。
2. **SSE 结束标记**：OpenAI 为 `data: [DONE]`，Anthropic 为 `message_stop` 事件，Ollama 是无标记的流结束；统一转为 `Finish`。
3. **空 delta 与心跳**：忽略空 content 帧，避免 UI 抖动。
4. **reasoning 内容**：DeepSeek-R1 / o 系列返回 `reasoning_content`，映射为 `Reasoning` 事件，UI 折叠展示。
5. **错误体差异**：`{"error":{"message":...}}` vs `{"error":{...}}` vs 纯文本；统一抽取为 `UpstreamError{status, code, message, raw}`。
6. **超长请求**：部分网关对 header/body 有硬限制，需支持 `max_output_tokens` 与消息裁剪。
7. **CORS / 代理**：由 Rust 侧发起请求，天然无 CORS 问题；代理走 `reqwest::Proxy`。

---

## 2. 上下文组装

### 2.1 上下文来源

| 来源 | 触发方式 | 内容形态 |
|---|---|---|
| 显式文件引用 | `@path/to/file.ts` | 全文（超阈值则摘要或按符号裁剪） |
| 文件夹引用 | `@src/api/` | 目录树 + 每个文件的头部签名 |
| 选区引用 | 选中代码后 `@selection` | 带行号范围的代码块 |
| 当前文件 | 自动（可关） | 光标所在文件的窗口上下文（前后各 N 行） |
| 符号引用 | `@symbol:parseConfig` | 由 tree-sitter 定位定义与引用点 |
| Git 变更 | `@git-diff` | 工作区未提交 diff |
| 终端输出 | `@terminal` | 最近 N 行输出 |
| 诊断信息 | `@problems` | 当前错误与告警列表 |

### 2.2 组装管线

```
用户输入 + 引用列表
   ↓ 解析 @ 引用 → 拉取内容
   ↓ 去重、按优先级排序（选区 > 显式文件 > 自动上下文）
   ↓ token 计数（tiktoken-rs，按模型选择编码器）
   ↓ 超预算？→ 分层压缩
   ↓ 生成最终 messages[]
```

### 2.3 压缩策略（按优先级依次尝试）

| 顺序 | 策略 | 说明 |
|---|---|---|
| 1 | 丢自动上下文 | 先牺牲非显式引用 |
| 2 | 折叠历史轮次 | 保留最近 N 轮完整，更早的仅保留用户消息 |
| 3 | 文件按符号裁剪 | 大文件只保留与用户意图相关的符号（tree-sitter + 关键词匹配） |
| 4 | 历史摘要 | 用轻量模型把早期轮次压成结构化摘要（决策、改动、未决项） |
| 5 | 拒绝并提示 | 仍超限则明确告知用户，给出"缩小引用范围"的建议 |

**预算分配建议**：system prompt ≤ 5%，引用内容 ≤ 40%，历史 ≤ 45%，留给输出 ≥ 10%。

---

## 3. Agent 循环

### 3.1 主循环（ReAct / Tool-Calling 混合）

```
run(goal):
  messages = [system] + context + [user(goal)]
  for step in 1..=max_steps:
      stream = provider.chat_stream(messages, tools)
      收集 → assistant_message (text + tool_calls)
      messages.append(assistant_message)

      if no tool_calls:
          return completed          # 模型自然收尾

      for call in tool_calls:
          if needs_approval(call):  emit approval_required → 等 agent.decide
              if denied: messages.append(tool_result(denied)) ; continue
          if call.name in {"write_file","edit_file","create_file","delete_path"}:
              checkpoint(call)      # 打快照（用于回滚）
          result = execute(call)    # 带超时、截断、审计
          emit tool_finished
          messages.append(tool_result(result))

      if step == max_steps:
          return failed(reason="step_limit")
```

### 3.2 循环控制参数

| 参数 | 默认 | 说明 |
|---|---|---|
| `max_steps` | 25 | 单次运行最大工具轮次 |
| `tool_timeout_ms` | 120000 | 单次命令执行超时 |
| `output_cap_bytes` | 65536 | 单次工具输出截断上限 |
| `no_progress_limit` | 3 | 连续 N 轮无文件变更/无新信息的自动中止 |
| `max_parallel_tools` | 3 | 只读工具可并行；写操作串行 |

### 3.3 停止条件

1. 模型不再请求工具 → 完成。
2. 达到 `max_steps` → 失败并给出已完成部分。
3. 用户点击停止 → `cancelled`。
4. 连续同类工具失败 ≥ 3 次 → 失败并建议用户介入。
5. 预算耗尽（token / 成本上限，可配）→ 失败。

---

## 4. 工具实现规范

### 4.1 统一工具签名

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;   // JSON Schema
    fn risk(&self) -> RiskLevel;                 // Low / Medium / High
    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutcome>;
}

pub struct ToolOutcome {
    pub ok: bool,
    pub summary: String,        // 给模型看的紧凑结果（受预算限制）
    pub detail: Option<String>, // 给 UI 展开看的完整输出
    pub artifacts: Vec<String>, // 产生的文件路径
    pub extra: Value,
}
```

### 4.2 各工具要点

| 工具 | 关键实现点 |
|---|---|
| `read_file` | 支持行范围；超 200KB 或 5000 行自动截断并提示；二进制文件拒读并提示 |
| `list_dir` | 默认不递归；尊重 `.gitignore`；结果数量上限并提示截断 |
| `grep` | 基于 ripgrep 语义；默认忽略二进制与 `.gitignore`；默认上限 200 条命中 |
| `write_file` | 整文件写入；写入前必须完成 diff 预览与审批；带乐观锁 hash |
| `edit_file` | **精确串替换**：`old_string` 必须在文件中**唯一存在**（除非 `replace_all`）；不唯一则返回 `3003` 并把候选位置回灌给模型重试 |
| `create_file` | 目标已存在则拒绝，要求改用 `write_file` |
| `delete_path` | 强制走系统回收站；工作区内才允许；高风险审批 |
| `run_command` | `cmd` 与 `args` 分离（不拼 shell 字符串）；`cwd` 必须在工作区内；超时强杀进程树；stdout/stderr 实时流式回传 |

### 4.3 `edit_file` 的抗幻觉设计（关键）

模型最容易在这里翻车。策略：

1. **要求唯一匹配**：不唯一 → 拒绝执行，回灌"该片段出现 N 次，请提供更多上下文以唯一定位"。
2. **要求提供足够上下文**：prompt 中明确要求 `old_string` 至少包含 3 行上下文。
3. **空白容错**：对比时忽略行尾空白差异，但应用时保持原有缩进风格。
4. **失败即回灌**：把失败原因 + 文件相关片段回给模型，让它自动重试（最多 2 次，之后转人工）。
5. **应用即校验**：应用后立即重新解析确认语法结构未被破坏（tree-sitter error node 检测），异常则告警。

---

## 5. Prompt 架构

### 5.1 系统提示词分层

```
[身份与准则]  —— 你是嵌入在 AI IDE 中的编程助手……
[环境信息]    —— 操作系统、工作区根路径、当前打开文件、Shell 类型、项目语言构成
[工具使用规范] —— 何时用哪个工具；写文件前必须先读；小步修改优于整文件重写
[输出规范]    —— 面向用户的自然语言用中文/跟随用户；代码块标注语言
[安全约束]    —— 不执行破坏性命令；不访问工作区外路径；不确定时先询问
```

### 5.2 模式差异

| 模式 | 工具集 | 行为约束 |
|---|---|---|
| Chat | 只读工具（read/list/grep） | 只回答与解释，不主动改文件 |
| Agent | 全量工具 | 允许修改与执行，受权限策略约束 |
| Inline | 无工具 | 只输出替换选区的新代码，不解释不寒暄 |

### 5.3 项目管理

- Prompt 以模板文件存放在 `crates/agent/prompts/*.hbs`，支持变量插值，便于版本化与 A/B 对比。
- 每次请求把实际使用的 prompt 版本号写入 `tool_audit` 关联记录，便于回溯效果差异。

---

## 6. 权限与审批

### 6.1 风险分级

| 级别 | 判定 | 默认策略 |
|---|---|---|
| Low | 只读（read/list/grep）、只读命令（`git status`、`ls`） | `always_allow` |
| Medium | 写文件、创建文件、格式化、安装依赖 | `ask_every_time`（可升为 always_allow） |
| High | 删除、执行任意命令、修改工作区外、联网请求 | `ask_every_time`，不允许一键 always_allow（仅可"本次允许"） |

### 6.2 审批 UI 契约

审批卡片必须展示：

1. 工具名与风险徽标；
2. **完整参数**（命令则展示 `cmd + args` 展开后的可读形式）；
3. 影响范围（涉及哪些文件/目录）；
4. 动作按钮：`允许一次` / `本次会话始终允许` / `拒绝` / `修改后执行`（可编辑参数）。

### 6.3 命令黑名单（内置，不可关闭）

```
format, diskpart, shutdown /r /s, reg delete, net user, 
Remove-Item -Recurse -Force C:\, rd /s /q C:\, 
rm -rf /, mkfs, dd if=*, :(){ :|:& };:
```
命中即 High 风险，且**禁止**"始终允许"。

---

## 7. 检查点与回滚

### 7.1 快照策略

- 触发点：任何写类工具执行**之前**。
- 内容：受影响文件的原始内容（压缩存储于 `cache/checkpoints/<session>/<cp_id>/`）+ 元数据。
- 去重：同一文件在同一 checkpoint 内只存一次。
- 清理：会话关闭后保留 7 天，或用户手动清理。

### 7.2 回滚语义

| 操作 | 行为 |
|---|---|
| `agent.undo(单步)` | 撤销最后一次工具调用造成的全部文件变更 |
| `agent.undo(run_id)` | 撤销整轮 Agent 运行的所有变更 |
| 冲突处理 | 若文件在快照后被用户手动修改，拒绝覆盖并提示用户选择保留哪一份 |

---

## 8. MCP 集成

### 8.1 配置

```jsonc
// %APPDATA%/ai-ide/mcp.json
{
  "mcpServers": {
    "filesystem": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "E:\\proj"], "env": {} },
    "github":     { "type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": { "Authorization": "Bearer ${GITHUB_TOKEN}" } }
  }
}
```

### 8.2 工具注入规则

- 命名空间：`mcp__<server>__<tool>`，避免与内置工具冲突。
- 描述透传：MCP Server 的 `description` 与 `inputSchema` 原样注入模型的 `tools`。
- 权限：默认 `ask_every_time`，用户可按 server 批量放行；`always_allow` 仅对只读类工具开放。
- 失败隔离：某 Server 连接失败不影响其他 Server 与内置工具，UI 标记为"未连接"并给出原因。

---

## 9. 效果评估与可观测性

| 指标 | 采集方式 | 目标 |
|---|---|---|
| 首 token 延迟 | `llm.started` → 首个 `llm.delta` | P50 < 1.5s（不含上游排队） |
| 工具调用成功率 | `agent.tool_finished.ok` 统计 | ≥ 95% |
| 编辑应用成功率 | `edit_file` 唯一匹配命中率 | ≥ 90% |
| 任务完成率 | `agent.finished.status == completed` | 内部基线集 ≥ 70% |
| 平均步数 | `agent.finished.steps` | 简单任务 < 4 步 |
| 人工介入率 | `approval_required` → deny/编辑参数 | 监控趋势而非硬指标 |

**评估集**：构建 20 个真实小任务（跨 4 种语言）作为回归基线，每次改动 Agent 逻辑后跑一遍，记录完成率与步数变化。
