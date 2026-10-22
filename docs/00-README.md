---
AIGC:
    Label: "1"
    ContentProducer: 001191440300708461136T1XGW3
    ProduceID: 04526fdd866a8c5c9aceee6b83ed2c05_9ebb950dcc2511f1b76f525400638852
    ReservedCode1: Z+N/W7l7xLhUhT08wX5NFzPkx/KwubLMGT5AGxir3HMbBbVpBv7uQG9NJHmOepx2IKIB6dbUYR48yoOWQu5GvZl/lQCmGAVuHBquq4tcYGFcM8s/U9zOBEsjDW3UbMeOEAsrzNDwXuvEJorScoM2vdpQysKm1vybRfyX7Vw7DDJRGdRFOSZRY5jEia4=
    ContentPropagator: 001191440300708461136T1XGW3
    PropagateID: 04526fdd866a8c5c9aceee6b83ed2c05_9ebb950dcc2511f1b76f525400638852
    ReservedCode2: Z+N/W7l7xLhUhT08wX5NFzPkx/KwubLMGT5AGxir3HMbBbVpBv7uQG9NJHmOepx2IKIB6dbUYR48yoOWQu5GvZl/lQCmGAVuHBquq4tcYGFcM8s/U9zOBEsjDW3UbMeOEAsrzNDwXuvEJorScoM2vdpQysKm1vybRfyX7Vw7DDJRGdRFOSZRY5jEia4=
---

# AI IDE · 设计文档集

模型可插拔的 AI 原生代码编辑器。技术栈：**Electron + React + TypeScript（外壳/UI） + Rust（内核）**。

> 文档状态：v0.1（设计基线，待审阅）
> 实现状态：**M0 / M1 / M2 / M3 已完成并通过实测验收；M4（Agent 闭环）内核与外壳主线已完成**（Agent 循环与 8 个工具、权限闸门四动作、`agent.*` 协议与 `crates/core` 接线、外壳运行面板与审批 UI），遗留工具调用独立卡片组件、协议文档一致性巡检与多模型复测，详见 [10-M4-验收报告.md](./10-M4-验收报告.md)；**M5（持久化与治理）已完成**：会话 / 消息持久化层 `crates/store`、`session.*` 九个方法与只读 `audit.list` 已在内核接线并通过端到端测试（含杀进程重启后会话与消息完整恢复、审计留痕在会话删除后仍存活）；同一轮补齐三项 M5 缺口——检查点回滚（`checkpoint.create` / `list` / `restore` / `delete`，快照按 SHA-256 逐文件复核）、JSONL 结构化日志（按天滚动、出口脱敏、32 MiB 滚动上限，并在 CI 加密钥扫描门禁）、权限表持久化（`permission.list` / `revoke`，`allow_always` 落 `permission_rule` 表），能力清单 30 → 46，外壳会话面板、检查点面板与权限面板均已接进渲染端（详见 [11-M5-验收报告.md](./11-M5-验收报告.md)）；剩余未做仅消息级分叉入口与列表 / 审计的「加载更多」；**M6（终端 + Git）切片一（内核终端）与切片二（外壳终端面板）已完成**：`crates/terminal`（`portable-pty` 之上的 `TerminalManager`，多会话 / resize 夹取 / 子进程退出清理 / 单一事件通道）、`terminal.create` / `write` / `resize` / `kill` / `list` 五方法与 `terminal.data` / `terminal.exit` 两事件已进协议 `IMPLEMENTED` 并由 `crates/core/src/terminal_api.rs` 挂进 dispatch，能力清单 46 → **51**，单测 13 项 + 端到端 8 项（`crates/core/tests/terminal_flow.rs`）；外壳侧 `apps/desktop` 三层接通 xterm.js（main 事件路由与策略放行、preload 两个订阅方法、renderer 的 `terminal-api.ts` / `terminal-store.ts` / `TerminalPanel.tsx`，多标签 + FitAddon 尺寸同步 + 退出态重启，入口在活动栏与会话数角标、``Ctrl+` `` 开关），前端单测 23 项；Git（`git.*`）与真实 UI 主观体验验收未做，M7 未开工，M8 仅打包链路先行落地、验收未做（截至 2026-10-22）。本套文档撰写于编码之前，部分表述已与实现不符，凡有冲突以本文「实现状态」章节为准。

---

## 文档索引

| 文档 | 内容 |
|---|---|
| [01-需求规格说明.md](./01-需求规格说明.md) | 项目定位、用户场景、功能需求（P0/P1/P2）、非功能需求、竞品对标、MVP 边界 |
| [02-系统架构设计.md](./02-系统架构设计.md) | 三层架构、进程模型、通信链路、目录结构、Rust 模块划分、存储设计、ADR、安全模型、技术栈 |
| [03-内核接口协议规范.md](./03-内核接口协议规范.md) | JSON-RPC 2.0 协议全文：方法目录、事件、错误码、工具 schema、典型时序 |
| [04-AI能力与Agent设计.md](./04-AI能力与Agent设计.md) | Provider 抽象层、上下文组装与压缩、Agent 循环、工具实现、Prompt 架构、权限审批、检查点、MCP |
| [05-里程碑与验证计划.md](./05-里程碑与验证计划.md) | M0–M8 里程碑、任务分解、验收硬指标、测试策略、风险对策、待决问题 |
| [06-M0-验收报告.md](./06-M0-验收报告.md) | M0 实测验收：交付物、验收结果、一键复跑命令 |
| [07-M1-验收报告.md](./07-M1-验收报告.md) | M1 实测验收：编辑器壳功能面、e2e 19 条断言、缺陷复盘、遗留项 |
| [08-M2-验收报告.md](./08-M2-验收报告.md) | M2 实测验收：内核 + 外壳全链路、门禁实测、三条硬指标对照、缺陷复盘、M3 起点 |
| [09-M3-验收报告.md](./09-M3-验收报告.md) | M3 实测验收：引用注入与预算裁剪、精确计数与压缩分流、硬指标对照、缺陷复盘、三处限制 |
| [10-M4-验收报告.md](./10-M4-验收报告.md) | M4 实测验收：Agent 循环与 8 个工具、权限闸门四动作、真实模型联调、缺陷复盘、八处限制 |
| [11-M5-验收报告.md](./11-M5-验收报告.md) | M5 实测验收：会话 / 消息持久化、导出与分叉、审计与脱敏、检查点回滚、JSONL 日志与密钥扫描门禁、权限表持久化、外壳会话面板、验收标准对照、缺陷复盘、限制清单 |

## 实现状态（截至 2026-10-22）

### 里程碑进度

| 里程碑 | 主题 | 状态 |
|---|---|---|
| M0 | 骨架与通信 | 已完成，实测见 [06-M0-验收报告.md](./06-M0-验收报告.md) |
| M1 | 编辑器壳 | 已完成，实测见 [07-M1-验收报告.md](./07-M1-验收报告.md) |
| M2 | 模型接入 | 已完成（内核 + 外壳）：`provider.*` / `model.*` / `llm.*` 共新增 11 个请求方法，能力清单方法数 16 → 27；Provider 设置面板、密钥 `safeStorage` 托管、流式对话 / 取消 / 错误分类全链路可跑，实测见 [08-M2-验收报告.md](./08-M2-验收报告.md)。遗留：`provider.test` 与流式首 token 时延未对真实厂商实测（M4 联调阶段已用真实网关跑通流式对话链路，见 [10-M4-验收报告.md](./10-M4-验收报告.md)） |
| M3 | 上下文引用 | 已完成：`context.refs` 注入与预算裁剪、`truncate` / 离线 `summarize` 分流、tiktoken 按模型族精确计数，实测见 [09-M3-验收报告.md](./09-M3-验收报告.md)。遗留：tree-sitter 符号表、模型摘要；tiktoken 本地计数与上游 `usage` 未做端到端比对 |
| M4 | Agent 闭环 | 已完成（内核 + 外壳主线）：`crates/agent` 循环调度与 8 个工具、权限闸门四动作、`agent.start` / `stop` / `decide` 协议与 `crates/core` 接线、外壳运行面板与审批卡片，实测见 [10-M4-验收报告.md](./10-M4-验收报告.md)。遗留：工具调用独立卡片组件、协议文档一致性巡检、多模型复测与多次采样 |
| M5 | 持久化与治理 | 已完成：`crates/store`（sqlx + SQLite 会话 / 消息 / 审计 / 权限规则仓储，含事务内 `fork_session` 与审计脱敏留存）、`crates/protocol` 九个 `session.*` + 只读 `audit.list` + 四个 `checkpoint.*` + 两个 `permission.*` 及 DTO（能力清单 30 → 46）、`crates/core` 的 `session_api.rs` / `session_export.rs` / `audit_api.rs` / `checkpoint_api.rs` / `permission_api.rs` 接线、JSONL 结构化日志（`logging.rs`）、外壳 `session-store.ts` + `session-bridge.ts` + `SessionPanel.tsx` / `CheckpointPanel.tsx` / 权限面板，对话侧首条消息自动建会话、流结束按用量落 `assistant`；实测见 [11-M5-验收报告.md](./11-M5-验收报告.md)。未做：消息级分叉入口、列表与审计的「加载更多」 |
| M6 | 终端与 Git | 切片一（内核终端）与切片二（外壳终端面板）已完成，详见 [05-里程碑与验证计划.md](./05-里程碑与验证计划.md)；`git.*` 与 Git UI 未做 |
| M7 | MCP | 未开工 |
| M8 | 打包 | 打包链路（electron-builder + 打包脚本）已落地，产出 `AI-IDE-0.1.0-win-x64.exe`（core 二进制在 asar 外），干净环境安装验收未做 |

### 已落地的技术栈（实测）

| 层 | 实际落地 | 备注 |
|---|---|---|
| 壳 | Electron 44 + electron-vite 6 + Vite 8 | main / preload / renderer 三端齐备 |
| UI | React 19 + TypeScript 5.6 + Monaco 0.57 | 状态管理为手写 store（`useSyncExternalStore` 订阅），未引入第三方状态库 |
| 样式 | 手写 CSS | 未引入 Tailwind / Radix 等 UI 框架 |
| 测试 | Vitest 3 + playwright-core | 前端单测 + Electron 真界面 E2E |
| 内核 | Rust 2021（rust-version 1.79）+ tokio | 七个 crate：`protocol` / `workspace` / `indexer` / `llm` / `agent` / `store` / `core` |
| 通信 | JSON-RPC 2.0 over stdio（NDJSON） | 帧与 DTO 定义在 `crates/protocol`，TS 类型由 ts-rs 生成 |

> `02-系统架构设计.md` 中的技术栈清单（React 18、Zustand、Tailwind、Radix、features 分层等）与实测**存在偏差**，属待拍板事项，未擅自改写。

### 已实现的能力面

- `core.*`：`hello` / `ping` / `stats` / `shutdown`
- `workspace.*`：`open` / `close` / `tree` / `read_file` / `write_file` / `create` / `rename` / `delete` / `reveal`
- `search.*`：`files`（文件名模糊）、`content`（内容检索，支持正则）
- `session.*`：`create` / `list` / `get` / `update` / `delete` / `append_message` / `messages` / `export`（Markdown / JSON，`out_path` 由系统保存对话框决定）/ `fork`；`audit.list` 只读查询（会话创建 / 更新 / 删除 / 导出 / 分叉留痕，存储层已脱敏）
- `checkpoint.*`：`create`（手动快照，或由 Agent 写入钩子自动留痕）/ `list` / `restore`（回滚后逐文件重算 SHA-256 复核，结果折成 `verified`）/ `delete`；快照内容落 `<data_dir>/checkpoints/<工作区键>/<检查点 id>/`，单文件 8 MiB、单检查点 5000 文件 / 256 MiB 上限，超限计入 `skipped`
- `permission.*`：`list` / `revoke`；`allow_always` 决定经 `permission_rule` 表持久化（迁移 `0003_permission_rule.sql`），重启后仍生效，可按工作区撤销
- 日志：`<data_dir>/logs/core-YYYY-MM-DD.jsonl`（一行一条 JSON，按天滚动 + 单文件 32 MiB 上限），出口复用 `ai_ide_store::redact` 的同一套脱敏规则；`AI_IDE_LOG_JSONL` / `AI_IDE_LOG_REDACT` / `AI_IDE_LOG_DIR` / `AI_IDE_LOG_PROBE` 四个开关
- `provider.*` / `model.*`：`upsert` / `list` / `delete`（provider 另有 `test`）；目录落盘 `AI_IDE_DATA_DIR/providers.json`，密钥经 `ai-ide-llm` 的 `secret` 模块加密后存储
- `llm.*`：`chat_stream`（流式，支持 `context.refs` 注入为前置 `system` 消息）、`cancel`、`list_models`、`count_tokens`（按模型族分流 tiktoken 精确计数 / 估算回落，`messages[]` 与 `texts[]` 相加）；上游协议为 OpenAI Chat Completions 兼容
- 事件：`workspace.changed`、`provider.changed`、`llm.started` / `delta` / `reasoning` / `tool_call` / `usage` / `finished` / `error`
- 外壳能力：主进程策略闸门、内核进程监管与优雅退出、preload 能力收窄、渲染端工作区/文件树/多标签/搜索状态机与 Monaco 集成；M5 会话面板、检查点面板（列表 / 手动快照 / 回滚确认 / 删除）与权限面板（列表 / 撤销）及落盘桥（导出落点只经系统保存对话框、白名单按方法逐个放行）

### 尚未实现（设计图纸状态）

- 协议面：`terminal.*` / `git.*` / `mcp.*` / `search.symbols` / `workspace.indexed` / `core.log` 主动推送（`DECLARED_NOT_IMPLEMENTED` 目前为空表，这些方法尚未进协议常量表；M4 的 `agent.*` 与 M5 的 `checkpoint.*` / `permission.*` 已接线并进入 `IMPLEMENTED`）
- 模块：`mcp` / `terminal` / `parser`
- 界面：M2 的 Provider 配置 / 对话面板、M3 的 `@` 补全与引用 chip、M4 的 Agent 运行面板与审批卡片、M5 的会话 / 检查点 / 权限面板均已接入渲染端；Inline Edit（Ctrl+K）、Tab 补全、LSP、列表与审计分页加载、消息级分叉入口未开工

### 质量门禁实测（2026-10-22，M5 收尾 + P1 文档与 CI 收口）

`cargo test --workspace` **685 passed / 0 failed / 9 ignored**（27 个测试目标）；`cargo clippy --workspace --all-targets -- -D warnings` 退出码 0（另有 ts-rs 的 `failed to parse serde attribute` 构建期噪声，非本项目代码）；`cargo fmt --all -- --check` 通过；`pnpm test` **181 用例全过**（10 文件）；`pnpm scan:secrets` 通过（检测器自检 4 条正样本命中 / 5 条脱敏形态放行，内核探针日志 1 文件 4 行无明文）；`pnpm smoke:core` 6/6；`pnpm smoke:m1` 23/23；`pnpm e2e:m1` 19/19；`pnpm lint` / `pnpm typecheck` 零问题；`pnpm --filter @ai-ide/desktop build` 成功；`pnpm types:gen` 无漂移。CI 定义已含上述步骤（含日志密钥扫描步骤，跑的是 `node scripts/scan-log-secrets.mjs --probe`），可用 `pnpm verify:ci` 本地等价复现（**远端 Actions 尚未真正执行**）；M5 专项：`pnpm verify:m5`。

## 建议阅读顺序

- **只关心做什么** → 01
- **关心怎么搭** → 02 → 03
- **关心 AI 部分** → 04
- **关心怎么推进** → 05

---

## 一句话架构

```
React UI  ←IPC→  Electron Main（策略闸门）  ←stdio JSON-RPC→  Rust Core（能力实现与模型 I/O）
```

## 核心设计主张

1. **模型中立**：Provider 抽象层消化所有厂商协议差异，用户自带任意 API。
2. **进程隔离**：内核独立进程，崩溃可重启、可单测、不拖垮 UI。
3. **契约单一来源**：协议定义在 Rust，TS 类型自动生成，杜绝两端漂移。
4. **安全双闸门**：Main 判策略、Core 校验边界，模型输出一律视为不可信。
5. **改动可回滚**：写操作前打检查点，diff 强制预览，误改可精确还原。
*（内容由AI生成，仅供参考）*
