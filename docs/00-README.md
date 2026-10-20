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
> 实现状态：**M0 / M1 / M2 已完成并通过实测验收；M3 已完成（上下文引用注入与预算裁剪、`@` 引用外壳链路、编辑器侧引用、`llm.count_tokens` 精确计数、`compress.strategy` 分流），M4 起尚未开工**（截至 2026-10-20）。本套文档撰写于编码之前，部分表述已与实现不符，凡有冲突以本文「实现状态」章节为准。

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

## 实现状态（截至 2026-10-19）

### 里程碑进度

| 里程碑 | 主题 | 状态 |
|---|---|---|
| M0 | 骨架与通信 | 已完成，实测见 [06-M0-验收报告.md](./06-M0-验收报告.md) |
| M1 | 编辑器壳 | 已完成，实测见 [07-M1-验收报告.md](./07-M1-验收报告.md) |
| M2 | 模型接入 | 已完成（内核 + 外壳）：`provider.*` / `model.*` / `llm.*` 共新增 11 个请求方法，能力清单方法数 16 → 27；Provider 设置面板、密钥 `safeStorage` 托管、流式对话 / 取消 / 错误分类全链路可跑，实测见 [08-M2-验收报告.md](./08-M2-验收报告.md)。遗留：真实厂商接口与首 token 时延未实测 |
| M3 | 上下文引用 | 已完成：`context.refs` 注入与预算裁剪、`truncate` / 离线 `summarize` 分流、tiktoken 按模型族精确计数，实测见 [09-M3-验收报告.md](./09-M3-验收报告.md)。遗留：tree-sitter 符号表、模型摘要、真实上游未验证 |
| M4–M8 | Agent 闭环 / 持久化 / 终端与 Git / MCP / 打包 | 未开工 |

### 已落地的技术栈（实测）

| 层 | 实际落地 | 备注 |
|---|---|---|
| 壳 | Electron 44 + electron-vite 6 + Vite 8 | main / preload / renderer 三端齐备 |
| UI | React 19 + TypeScript 5.6 + Monaco 0.57 | 状态管理为手写 store（`useSyncExternalStore` 订阅），未引入第三方状态库 |
| 样式 | 手写 CSS | 未引入 Tailwind / Radix 等 UI 框架 |
| 测试 | Vitest 3 + playwright-core | 前端单测 + Electron 真界面 E2E |
| 内核 | Rust 2021（rust-version 1.79）+ tokio | 五个 crate：`protocol` / `workspace` / `indexer` / `llm` / `core` |
| 通信 | JSON-RPC 2.0 over stdio（NDJSON） | 帧与 DTO 定义在 `crates/protocol`，TS 类型由 ts-rs 生成 |

> `02-系统架构设计.md` 中的技术栈清单（React 18、Zustand、Tailwind、Radix、features 分层等）与实测**存在偏差**，属待拍板事项，未擅自改写。

### 已实现的能力面

- `core.*`：`hello` / `ping` / `stats` / `shutdown`
- `workspace.*`：`open` / `close` / `tree` / `read_file` / `write_file` / `create` / `rename` / `delete` / `reveal`
- `search.*`：`files`（文件名模糊）、`content`（内容检索，支持正则）
- `provider.*` / `model.*`：`upsert` / `list` / `delete`（provider 另有 `test`）；目录落盘 `AI_IDE_DATA_DIR/providers.json`，密钥经 `ai-ide-llm` 的 `secret` 模块加密后存储
- `llm.*`：`chat_stream`（流式，支持 `context.refs` 注入为前置 `system` 消息）、`cancel`、`list_models`、`count_tokens`（按模型族分流 tiktoken 精确计数 / 估算回落，`messages[]` 与 `texts[]` 相加）；上游协议为 OpenAI Chat Completions 兼容
- 事件：`workspace.changed`、`provider.changed`、`llm.started` / `delta` / `reasoning` / `tool_call` / `usage` / `finished` / `error`
- 外壳能力：主进程策略闸门、内核进程监管与优雅退出、preload 能力收窄、渲染端工作区/文件树/多标签/搜索状态机与 Monaco 集成

### 尚未实现（设计图纸状态）

- 协议面：`session.*` / `agent.*` / `terminal.*` / `git.*` / `mcp.*` / `permission.*` / `search.symbols` / `workspace.indexed` / `core.log` 主动推送（`DECLARED_NOT_IMPLEMENTED` 目前为空表，这些方法尚未进协议常量表）
- 模块：`crates/agent` / `mcp` / `terminal` / `store` / `parser`（`crates/llm` 已创建，见上）
- 界面：M2 的 Provider 配置 / 对话面板与 M3 的 `@` 补全、引用 chip 已接入渲染端；Inline Edit（Ctrl+K）、Tab 补全、LSP、会话历史 UI 未开工

### 质量门禁实测（2026-10-20，M3 收尾）

`cargo test --workspace` **345 passed / 0 failed**；`cargo clippy --workspace --all-targets -- -D warnings` 零告警；`cargo fmt --all -- --check` 通过；`pnpm test` **118 用例全过**（7 文件）；`pnpm smoke:core` 6/6；`pnpm smoke:m1` 23/23；`pnpm e2e:m1` 19/19；`pnpm lint` / `pnpm typecheck` 零问题。CI 定义已含上述全部步骤，可用 `pnpm verify:ci` 本地等价复现。

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
