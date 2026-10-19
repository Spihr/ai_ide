---
AIGC:
    Label: "1"
    ContentProducer: 001191440300708461136T1XGW3
    ProduceID: 04526fdd866a8c5c9aceee6b83ed2c05_eb4cd658cad411f18533525400638852
    ReservedCode1: 6EJZ1hEO7i+bzZpB/1tgvk4DI/FFlOpRpiM126ht8ItYUyPpym2VjTws37SgmWYgiM7pAPbvfVkMBlV2m1XNbPXt0rCcGGJ2LikGU1YpX2Y+VskQBU6puMZqEo0baYHShYitHDKq9pUgv2dkDZDnUbrJewy4EdTXmgrc220q1+ZyeZxvX18wGFe6Azk=
    ContentPropagator: 001191440300708461136T1XGW3
    PropagateID: 04526fdd866a8c5c9aceee6b83ed2c05_eb4cd658cad411f18533525400638852
    ReservedCode2: 6EJZ1hEO7i+bzZpB/1tgvk4DI/FFlOpRpiM126ht8ItYUyPpym2VjTws37SgmWYgiM7pAPbvfVkMBlV2m1XNbPXt0rCcGGJ2LikGU1YpX2Y+VskQBU6puMZqEo0baYHShYitHDKq9pUgv2dkDZDnUbrJewy4EdTXmgrc220q1+ZyeZxvX18wGFe6Azk=
---

# AI IDE · 设计文档集

模型可插拔的 AI 原生代码编辑器。技术栈：**Electron + React + TypeScript（外壳/UI） + Rust（内核）**。

> 文档状态：v0.1（设计基线，待审阅）
> 实现状态：**M0 / M1 已完成并通过实测验收，M2 尚未开工**（截至 2026-10-19）。本套文档撰写于编码之前，部分表述已与实现不符，凡有冲突以本文「实现状态」章节为准。

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

## 实现状态（截至 2026-10-19）

### 里程碑进度

| 里程碑 | 主题 | 状态 |
|---|---|---|
| M0 | 骨架与通信 | 已完成，实测见 [06-M0-验收报告.md](./06-M0-验收报告.md) |
| M1 | 编辑器壳 | 已完成，实测见 [07-M1-验收报告.md](./07-M1-验收报告.md) |
| M2 | 模型接入 | 未开工 |
| M3–M8 | 上下文引用 / Agent 闭环 / 持久化 / 终端与 Git / MCP / 打包 | 未开工 |

### 已落地的技术栈（实测）

| 层 | 实际落地 | 备注 |
|---|---|---|
| 壳 | Electron 44 + electron-vite 6 + Vite 8 | main / preload / renderer 三端齐备 |
| UI | React 19 + TypeScript 5.6 + Monaco 0.57 | 状态管理为手写 store（`useSyncExternalStore` 订阅），未引入第三方状态库 |
| 样式 | 手写 CSS | 未引入 Tailwind / Radix 等 UI 框架 |
| 测试 | Vitest 3 + playwright-core | 前端单测 + Electron 真界面 E2E |
| 内核 | Rust 2021（rust-version 1.79）+ tokio | 四个 crate：`protocol` / `workspace` / `indexer` / `core` |
| 通信 | JSON-RPC 2.0 over stdio（NDJSON） | 帧与 DTO 定义在 `crates/protocol`，TS 类型由 ts-rs 生成 |

> `02-系统架构设计.md` 中的技术栈清单（React 18、Zustand、Tailwind、Radix、features 分层等）与实测**存在偏差**，属待拍板事项，未擅自改写。

### 已实现的能力面

- `core.*`：`hello` / `ping` / `stats` / `shutdown`
- `workspace.*`：`open` / `close` / `tree` / `read_file` / `write_file` / `create` / `rename` / `delete` / `reveal`
- `search.*`：`files`（文件名模糊）、`content`（内容检索，支持正则）
- 事件：`workspace.changed`
- 外壳能力：主进程策略闸门、内核进程监管与优雅退出、preload 能力收窄、渲染端工作区/文件树/多标签/搜索状态机与 Monaco 集成

### 尚未实现（设计图纸状态）

`llm.*` / `provider.*` / `model.*` / `session.*` / `agent.*` / `terminal.*` / `git.*` / `mcp.*` / `permission.*` / `search.symbols` / `workspace.indexed` / `core.log` 主动推送，以及 `crates/llm` / `agent` / `mcp` / `terminal` / `store` / `parser` 等模块。

### 质量门禁实测（2026-10-19）

`cargo test --workspace` 157 passed；`cargo clippy --workspace --all-targets -- -D warnings` 零告警；`cargo fmt --all -- --check` 通过；`pnpm test` 26 用例全过；`pnpm smoke:core` 6/6；`pnpm smoke:m1` 23/23；`pnpm e2e:m1` 19/19；`pnpm lint` 零问题（本轮新增 ESLint 配置后达成）。CI 定义已含上述全部步骤，可用 `pnpm verify:ci` 本地等价复现。

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
