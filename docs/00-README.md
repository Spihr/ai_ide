# AI IDE · 设计文档集

模型可插拔的 AI 原生代码编辑器。技术栈：**Electron + React + TypeScript（外壳/UI） + Rust（内核）**。

> 文档状态：v0.1（设计基线，待审阅）
> 实现状态：**M0 / M1 / M2 / M3 已完成并通过实测验收；M4（Agent 闭环）内核与外壳主线已完成**（Agent 循环与 8 个工具、权限闸门四动作、`agent.*` 协议与 `crates/core` 接线、外壳运行面板与审批 UI），遗留工具调用独立卡片组件、协议文档一致性巡检与多模型复测，详见 [10-M4-验收报告.md](./10-M4-验收报告.md)；**M5（持久化与治理）已完成**：会话 / 消息持久化层 `crates/store`、`session.*` 九个方法与只读 `audit.list` 已在内核接线并通过端到端测试（含杀进程重启后会话与消息完整恢复、审计留痕在会话删除后仍存活）；同一轮补齐三项 M5 缺口——检查点回滚（`checkpoint.create` / `list` / `restore` / `delete`，快照按 SHA-256 逐文件复核）、JSONL 结构化日志（按天滚动、出口脱敏、32 MiB 滚动上限，并在 CI 加密钥扫描门禁）、权限表持久化（`permission.list` / `revoke`，`allow_always` 落 `permission_rule` 表），能力清单 30 → 46，外壳会话面板、检查点面板与权限面板均已接进渲染端（详见 [11-M5-验收报告.md](./11-M5-验收报告.md)）；剩余未做仅消息级分叉入口与列表 / 审计的「加载更多」；**M6（终端 + Git）四个切片已完成**：`crates/terminal`（`portable-pty` 之上的 `TerminalManager`，多会话 / resize 夹取 / 子进程退出清理 / 单一事件通道）、`terminal.create` / `write` / `resize` / `kill` / `list` 五方法与 `terminal.data` / `terminal.exit` 两事件已进协议 `IMPLEMENTED` 并由 `crates/core/src/terminal_api.rs` 挂进 dispatch，能力清单 46 → **51**，单测 13 项 + 端到端 8 项（`crates/core/tests/terminal_flow.rs`）；外壳侧 `apps/desktop` 三层接通 xterm.js（main 事件路由与策略放行、preload 两个订阅方法、renderer 的 `terminal-api.ts` / `terminal-store.ts` / `TerminalPanel.tsx`，多标签 + FitAddon 尺寸同步 + 退出态重启，入口在活动栏与会话数角标、``Ctrl+` `` 开关），前端单测 23 项；切片三在 `crates/git` 落地内核 Git（驱动系统 `git` 子进程而非链接 `git2`，`status` / `diff` / `stage` / `commit` / `branches` 五方法 + 7001–7005 错误码，能力清单 51 → **56**，单测 36 项 + 端到端 6 项），切片四在 `apps/desktop` 接通外壳 Git 面板（`policy.ts` 放行五个 `git.*`、`git-api.ts` 解析 unified diff、`git-store.ts` 分组与暂存 / 提交状态机、`GitPanel.tsx` 变更列表 + diff 视图 + 提交框 + 分支条，入口挂活动栏「源代码管理」，前端 Git 测试 41 项、全量 248 项）；真实 Electron 窗口的人工点击验收与卡顿 / resize 主观体感实测未做（详见 [12-M6-验收报告.md](./12-M6-验收报告.md)），**M7（MCP）内核链路两个切片已完成**：`crates/mcp` 是不依赖内核的 JSON-RPC 2.0 客户端（NDJSON + stdio / HTTP(SSE) 双传输、握手协商、`tools/list` / `tools/call`、`mcp.json` 的 `${ENV}` 展开），`crates/core/src/mcp_manager.rs` 承接连接账本（幂等 `connect`、`disconnect` 后工具立即下架、内核退出 `shutdown_all`），`mcp.servers` / `connect` / `disconnect` / `tools` / `call` 五方法进 `IMPLEMENTED` 并配 `8001`–`8007` 错误码（`8003` 未连接必须早于 `8004` 工具不存在判定），已连接 Server 的工具按 `mcp__<server>__<tool>` 注入 agent 工具注册表且复用内建工具同一条权限闸门，能力清单 56 → **61**，端到端 `crates/core/tests/mcp_flow.rs` 6 项以真子进程 mock Server 跑通；**插件系统 P0–P5 已全部完成**：`plugin.list` / `status` / `install` / `uninstall` / `load` / `unload` / `reload` / `call` / `grant` / `revoke` / `command` 十一个方法与 `6101`–`6109` 错误码进 `IMPLEMENTED`（`plugin.command` 为全表第 72 个请求方法），能力清单 61 → **71** → **72**；逐阶段落点——P1 清单与安装（`crates/plugin` 三段校验 + `0004_plugin.sql` 账本，装 / 列 / 详情 / 卸不启进程）、P2 装载与握手（按行 NDJSON 帧、Windows 作业对象 `KILL_ON_JOB_CLOSE`、三级关停、非确定性退出自动重启且同次上限 3 次）、P3 工具注入（`plugin_tools.rs` 按命名空间注入并复用同一权限闸门）、P3.5 读侧宿主能力（`host/fs.read` / `list` / `stat` / `search.content` / `workspace.info`，闸门顺序能力 `6103` → 路径 `1002`/`1003` → 配额 `6109`）、P4 写侧与授权闭环（`host/fs.write` 原子落盘、`plugin.call` 六步定序、`grant` / `revoke` 运行态回写、四类审计事件）、P5 命令与事件、出站网络、模型调用（事件订阅投递、`plugin.command` 网关、全局命令面板、`host/progress`、`host/net.fetch` 三重闸门、`host/llm.chat` Provider 通道、外壳插件活动视图）；门禁实测 `cargo test --workspace` 1128 passed / 0 failed / 9 ignored、前端 21 文件 409 项、fmt / clippy / typecheck 均零告警，实测见 [23-插件-P5-验收报告.md](./23-插件-P5-验收报告.md)；码值没用 docs/14 原写的 `6001`–`6009`——那段已被终端占用，插件改占 `6101`–`6109` 并在 `error.rs` 加撞号断言；切片三补齐外壳（`policy.ts` 放行五个 `mcp.*`、`mcp-api.ts` / `mcp-store.ts` / `McpPanel.tsx`，活动栏「MCP Server」入口，Server 列表 + 连接 / 断开 + 工具清单 + 参数试跑，前端 MCP 测试 30 项、全量 280 项全绿）；「连接 2 个真实官方 Server」的验收已由 `pnpm e2e:m7`（23/23）与 `pnpm verify:m7` 覆盖（`f40cefb`）；M8 打包链路（electron-builder）已落地，2026-10-23 重打包（含 M7）后新增 `pnpm e2e:m8` 完成隔离用户配置下的运行验收 13/13（包内内核拉起、干净 profile 自建 `sessions.db` / `logs`、开工作区 → Monaco 读正文、MCP 面板空配置不报错、退出无残留内核），尚未在全新 Windows 上跑 NSIS 安装 / 卸载与缺失运行库验收（详见 [15-M8-验收报告.md](./15-M8-验收报告.md)）。**LSP 切片已收口**：内核 `crates/lsp`（stdio `Content-Length` 分帧、请求 id 票表、通知广播、握手、文档同步、错误分类）之上，`lsp.start` / `stop` / `servers` / `request` / `sync` 五方法进 `IMPLEMENTED`，诊断走 `lsp.diagnostics` 单向事件，能力清单 74 → **79**（`crates/protocol/src/methods.rs` 的 `IMPLEMENTED.len() == 79`、`DECLARED_NOT_IMPLEMENTED` 为空表）；外壳 `lsp-api.ts`（1-based / 0-based 换算、错误码翻人话）/ `lsp-store.ts`（按文件语言起连、缓冲同步、诊断按文件归档）/ `lsp-markers.ts`（诊断落 model 标记）/ `components/LspBridge.tsx`（开文件 / 保存 / 切文件 / 关工作区的生命周期）已接线，Monaco 定义 / 引用 / 悬停 / 补全四个 provider 由 `lsp-providers.ts` 的 `registerLspProviders` 在编辑器初始化时挂载（幂等，卸载时统一 dispose），命令面板新增「查看语言服务器状态」「重启语言服务器」；前端测试 `lsp-store.test.ts` 23 项 + `lsp-providers.test.ts` 16 项。本套文档撰写于编码之前，部分表述已与实现不符，凡有冲突以本文「实现状态」章节为准。

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
| [12-M6-验收报告.md](./12-M6-验收报告.md) | M6 实测验收：内核终端与 Git、外壳终端面板与 Git 面板、门禁实测、两条验收指标对照、缺陷复盘、七条限制清单 |
| [13-M7-验收报告.md](./13-M7-验收报告.md) | M7 实测验收：内核 MCP 客户端（双传输、握手、工具发现）、core 连接账本与 `mcp.*` 五方法、`mcp__<server>__<tool>` 工具注入与权限闸门复用、8xxx 错误码分段、门禁实测、验收指标对照、缺陷复盘、七条限制清单 |
| [14-插件系统设计.md](./14-插件系统设计.md) | 插件系统设计（**P0–P5 已落地**）：子进程 + stdio 形态选型、包规范与清单校验、`plugin.*` 网关与 6xxx 错误码、宿主 API 能力矩阵、四道闸门与配额、Agent 工具注入、接线清单、P0–P5 里程碑；配套可运行原型 `demo-plugin-system/`（12/12 场景通过） |
| [15-M8-验收报告.md](./15-M8-验收报告.md) | M8 实测验收：打包链路重打（含 M7）、`verify:package` 11 项、`e2e:m8` 隔离用户配置 13 项断言逐条结果、未做项（NSIS 安装 / 卸载 / 运行库 / 签名）、踩坑记录与后续待办 |

## 实现状态（截至 2026-10-25）

### 里程碑进度

| 里程碑 | 主题 | 状态 |
|---|---|---|
| M0 | 骨架与通信 | 已完成，实测见 [06-M0-验收报告.md](./06-M0-验收报告.md) |
| M1 | 编辑器壳 | 已完成，实测见 [07-M1-验收报告.md](./07-M1-验收报告.md) |
| M2 | 模型接入 | 已完成（内核 + 外壳）：`provider.*` / `model.*` / `llm.*` 共新增 11 个请求方法，能力清单方法数 16 → 27；Provider 设置面板、密钥 `safeStorage` 托管、流式对话 / 取消 / 错误分类全链路可跑，实测见 [08-M2-验收报告.md](./08-M2-验收报告.md)。遗留：`provider.test` 与流式首 token 时延未对真实厂商实测（M4 联调阶段已用真实网关跑通流式对话链路，见 [10-M4-验收报告.md](./10-M4-验收报告.md)） |
| M3 | 上下文引用 | 已完成：`context.refs` 注入与预算裁剪、`truncate` / 离线 `summarize` 分流、tiktoken 按模型族精确计数，实测见 [09-M3-验收报告.md](./09-M3-验收报告.md)。遗留：tree-sitter 符号表、模型摘要；tiktoken 本地计数与上游 `usage` 未做端到端比对 |
| M4 | Agent 闭环 | 已完成（内核 + 外壳主线）：`crates/agent` 循环调度与 8 个工具、权限闸门四动作、`agent.start` / `stop` / `decide` 协议与 `crates/core` 接线、外壳运行面板与审批卡片，实测见 [10-M4-验收报告.md](./10-M4-验收报告.md)。遗留：工具调用独立卡片组件、协议文档一致性巡检、多模型复测与多次采样 |
| M5 | 持久化与治理 | 已完成：`crates/store`（sqlx + SQLite 会话 / 消息 / 审计 / 权限规则仓储，含事务内 `fork_session` 与审计脱敏留存）、`crates/protocol` 九个 `session.*` + 只读 `audit.list` + 四个 `checkpoint.*` + 两个 `permission.*` 及 DTO（能力清单 30 → 46）、`crates/core` 的 `session_api.rs` / `session_export.rs` / `audit_api.rs` / `checkpoint_api.rs` / `permission_api.rs` 接线、JSONL 结构化日志（`logging.rs`）、外壳 `session-store.ts` + `session-bridge.ts` + `SessionPanel.tsx` / `CheckpointPanel.tsx` / 权限面板，对话侧首条消息自动建会话、流结束按用量落 `assistant`；实测见 [11-M5-验收报告.md](./11-M5-验收报告.md)。未做：消息级分叉入口、列表与审计的「加载更多」 |
| M6 | 终端与 Git | 已完成：`crates/terminal` + `terminal.*` 五方法与两事件、`crates/git` + `git.*` 五方法（能力清单 46 → 56）、外壳多标签终端与 Git 面板（活动栏入口 + 变更角标），实测见 [12-M6-验收报告.md](./12-M6-验收报告.md)；未做真实窗口人工验收与 M6 e2e 脚本 |
| M7 | MCP | 四个切片已完成：`crates/mcp` JSON-RPC 2.0 客户端（stdio + HTTP(SSE)、握手、`tools/list` / `tools/call`、`${ENV}` 展开）、`crates/core/src/mcp_manager.rs` 连接账本、`mcp.*` 五个方法与 `8001`–`8007` 错误码、按 `mcp__<server>__<tool>` 注入 agent 工具注册表并复用同一权限闸门（能力清单 56 → 61），端到端 `crates/core/tests/mcp_flow.rs` 6 项以真子进程 mock Server 跑通，全量 `cargo test --workspace` 917 passed / 0 failed；外壳 `policy.ts` 放行五个 `mcp.*` 并接通 `McpPanel`（Server 列表 / 连接断开 / 工具清单 / 参数试跑，活动栏「MCP Server」入口），前端 280 项全绿，实测见 [13-M7-验收报告.md](./13-M7-验收报告.md)；真实官方 Server 验收已由 `pnpm e2e:m7`（23/23）+ `pnpm verify:m7` 覆盖；未做真实 HTTP(SSE) 远程 Server 端到端与对话级自主选工具用例 |
| 语言服务 | LSP | **已完成（2026-10-25 收口）**：`crates/lsp` 客户端底座（stdio `Content-Length` 分帧、请求 id 票表与超时撕票、通知广播、握手、文档同步、错误可重连 / 致命分类）、`lsp.*` 五方法进 `IMPLEMENTED` 与 `lsp.diagnostics` 事件（能力清单 74 → **79**）、`crates/core/src/lsp_api.rs` 连接池按 `workspace_id → server_id` 复用；外壳 `lsp-api.ts` / `lsp-store.ts`（起连 / 缓冲同步 / 状态摘要 / 重启）/ `lsp-markers.ts`（诊断落 model 标记）/ `components/LspBridge.tsx` 生命周期接线，`lsp-providers.ts` 的 `registerLspProviders` 在编辑器初始化时挂载 Monaco 定义 / 引用 / 悬停 / 补全四个 provider，命令面板新增「查看语言服务器状态」「重启语言服务器」；未做语言服务器自动安装、格式化 / 重命名 provider、多根工作区多连接 |
| 插件系统 | P0–P5 | **已全部完成（2026-10-25，11 个提交，`09695c2`→`b5df5d3`）**：`plugin.*` 十一个方法进 `IMPLEMENTED`（P0 十个 + P5 `plugin.command`，后者为全表第 72 个请求方法），能力清单 61 → 71 → **72**，错误码 `6101`–`6109`（避开终端 `6001`–`6004`，`error.rs` 有段内不重复断言）；P1 清单与安装 → P2 装载 / 握手 / 崩溃重启（同次上限 3）→ P3 工具注入 → P3.5 读侧 `host/*`（能力→路径→配额三闸门）→ P4 写侧 `fs.write` + `plugin.call` + `grant` / `revoke` + 四类审计 → P5 事件投递 / `plugin.command` / 命令面板 / `host.progress` / `net.fetch` / `llm.chat` / 外壳活动视图；门禁 `cargo test --workspace` **1128 passed / 0 failed / 9 ignored**、前端 **21 文件 409 项**、fmt / clippy / typecheck 零告警，实测见 [23-插件-P5-验收报告.md](./23-插件-P5-验收报告.md) |
| M8 | 打包 | 打包链路（electron-builder + 打包脚本）已落地，产出 `AI-IDE-0.1.0-win-x64.exe`（core 二进制在 asar 外）；2026-10-23 重打包（含 M7）后 `pnpm verify:package` 11 项全 PASS、新增 `pnpm e2e:m8` 完成**隔离用户配置下的运行验收 13/13**（包内内核拉起、干净 profile 自建 `sessions.db` / `logs`、开工作区 → Monaco 读正文、MCP 面板空配置不报错、退出无残留内核），实测见 [15-M8-验收报告.md](./15-M8-验收报告.md)；**未做**：全新 Windows 上的 NSIS 安装 / 卸载 / 缺失运行库 / 代码签名验收 |

### 已落地的技术栈（实测）

| 层 | 实际落地 | 备注 |
|---|---|---|
| 壳 | Electron 44 + electron-vite 6 + Vite 8 | main / preload / renderer 三端齐备 |
| UI | React 19 + TypeScript 5.6 + Monaco 0.57 | 状态管理为手写 store（`useSyncExternalStore` 订阅），未引入第三方状态库 |
| 样式 | 手写 CSS | 未引入 Tailwind / Radix 等 UI 框架 |
| 测试 | Vitest 3 + playwright-core | 前端单测 + Electron 真界面 E2E |
| 内核 | Rust 2021（rust-version 1.79）+ tokio | 十二个 crate：`protocol` / `workspace` / `indexer` / `llm` / `agent` / `store` / `terminal` / `git` / `mcp` / `plugin` / `lsp` / `core` |
| 通信 | JSON-RPC 2.0 over stdio（NDJSON） | 帧与 DTO 定义在 `crates/protocol`，TS 类型由 ts-rs 生成 |

> `02-系统架构设计.md` 中的技术栈清单（React 18、Zustand、Tailwind、Radix、features 分层等）与实测**存在偏差**，属待拍板事项，未擅自改写。

### 已实现的能力面

- `core.*`：`hello` / `ping` / `stats` / `shutdown`
- `workspace.*`：`open` / `close` / `tree` / `read_file` / `write_file` / `create` / `rename` / `delete` / `reveal`
- `search.*`：`files`（文件名模糊）、`content`（内容检索，支持正则）、`symbols`（符号抽取：Rust / TypeScript / TSX / JavaScript / Python / Go / Java 的函数、方法、结构体 / 类、枚举、接口、trait 与常量，回相对路径与起止行号，可按 `kinds` 过滤）
- `terminal.*`：`create` / `write` / `resize` / `kill` / `list` 五方法 + `terminal.data` / `terminal.exit` 两事件（`crates/terminal`，`portable-pty`，退出含自行 `exit` 统一走事件并从列表摘除）
- `git.*`：`status` / `diff` / `stage` / `commit` / `branches`（驱动系统 `git` 子进程，工作区作用域、路径越界拒绝，错误码 7001–7005）
- `mcp.*`：`servers` / `connect` / `disconnect` / `tools` / `call`（stdio + HTTP(SSE) 双传输、`${ENV}` 展开，外部工具按 `mcp__<server>__<tool>` 注入 Agent 并复用同一权限闸门，错误码 8001–8007）
- `plugin.*`：`list` / `status` / `install` / `uninstall` / `load` / `unload` / `reload` / `call` / `grant` / `revoke` / `command` 十一方法（子进程 NDJSON 宿主、四级闸门、错误码 6101–6109）
- `lsp.*`：`start` / `stop` / `servers` / `request` / `sync` 五方法 + `lsp.diagnostics` 单向事件（`crates/lsp` 客户端底座，`crates/core/src/lsp_api.rs` 按 `workspace_id → server_id` 复用连接，查询方法限定为定义 / 引用 / 悬停 / 补全）
- `inline_edit.propose`：选区 + 上下文行 → 一次补全 → `replacement` / 结构化 `diff` / `usage`（内核不写盘，越界回 `9001`）
- `session.*`：`create` / `list` / `get` / `update` / `delete` / `append_message` / `messages` / `export`（Markdown / JSON，`out_path` 由系统保存对话框决定）/ `fork`；`audit.list` 只读查询（会话创建 / 更新 / 删除 / 导出 / 分叉留痕，存储层已脱敏）
- `checkpoint.*`：`create`（手动快照，或由 Agent 写入钩子自动留痕）/ `list` / `restore`（回滚后逐文件重算 SHA-256 复核，结果折成 `verified`）/ `delete`；快照内容落 `<data_dir>/checkpoints/<工作区键>/<检查点 id>/`，单文件 8 MiB、单检查点 5000 文件 / 256 MiB 上限，超限计入 `skipped`
- `permission.*`：`list` / `revoke`；`allow_always` 决定经 `permission_rule` 表持久化（迁移 `0003_permission_rule.sql`），重启后仍生效，可按工作区撤销
- 日志：`<data_dir>/logs/core-YYYY-MM-DD.jsonl`（一行一条 JSON，按天滚动 + 单文件 32 MiB 上限），出口复用 `ai_ide_store::redact` 的同一套脱敏规则；`AI_IDE_LOG_JSONL` / `AI_IDE_LOG_REDACT` / `AI_IDE_LOG_DIR` / `AI_IDE_LOG_PROBE` 四个开关
- `provider.*` / `model.*`：`upsert` / `list` / `delete`（provider 另有 `test`）；目录落盘 `AI_IDE_DATA_DIR/providers.json`，密钥经 `ai-ide-llm` 的 `secret` 模块加密后存储
- `llm.*`：`chat_stream`（流式，支持 `context.refs` 注入为前置 `system` 消息）、`cancel`、`list_models`、`count_tokens`（按模型族分流 tiktoken 精确计数 / 估算回落，`messages[]` 与 `texts[]` 相加）；上游协议为 OpenAI Chat Completions 兼容
- 事件：`workspace.changed`、`provider.changed`、`llm.started` / `delta` / `reasoning` / `tool_call` / `usage` / `finished` / `error`、`terminal.data` / `terminal.exit`、`lsp.diagnostics`、`plugin.*` 四类审计与进度事件
- 外壳能力：主进程策略闸门、内核进程监管与优雅退出、preload 能力收窄、渲染端工作区/文件树/多标签/搜索状态机与 Monaco 集成；M5 会话面板、检查点面板（列表 / 手动快照 / 回滚确认 / 删除）与权限面板（列表 / 撤销）及落盘桥（导出落点只经系统保存对话框、白名单按方法逐个放行）

### 尚未实现（截至 2026-10-25）

- 协议面：`workspace.indexed` / `core.log` 两个事件仍是主动推送缺口（`IMPLEMENTED` 现为 **79** 项、`DECLARED_NOT_IMPLEMENTED` 为空表；`terminal.*` / `git.*` / `mcp.*` / `plugin.*` / `lsp.*` / `search.symbols` / `inline_edit.propose` 均已进常量表并由 `crates/core` 接线，本文旧版把这几组写作「未进协议常量表」属倒挂，已订正）
- 模块：`parser`（tree-sitter 符号表尚未引入，`search.symbols` 走的是自研文本抽取）
- 界面：Tab 补全、列表与审计分页加载、消息级分叉入口未开工；已接入渲染端的有 M2 Provider 配置 / 对话面板、M3 `@` 补全与引用 chip、M4 Agent 运行面板与审批卡片、M5 会话 / 检查点 / 权限面板、M6 终端与 Git 面板、M7 MCP 面板、符号浮层（`Ctrl+T`）、Inline Edit 浮层（`Ctrl+K`）与 LSP 状态 / 重启命令
- LSP 侧：语言服务器需本机自备（按 `crates/lsp/src/registry.rs` 探测并探测失败即降级为「不可用」，未内置下载 / 安装）；格式化、重命名、代码动作等未接 Monaco provider；多根工作区只起工作区根那一份连接

### 质量门禁实测（2026-10-22，M5 收尾 + P1 文档与 CI 收口）

`cargo test --workspace` **685 passed / 0 failed / 9 ignored**（27 个测试目标）；`cargo clippy --workspace --all-targets -- -D warnings` 退出码 0（另有 ts-rs 的 `failed to parse serde attribute` 构建期噪声，非本项目代码）；`cargo fmt --all -- --check` 通过；`pnpm test` **181 用例全过**（10 文件）；`pnpm scan:secrets` 通过（检测器自检 4 条正样本命中 / 5 条脱敏形态放行，内核探针日志 1 文件 4 行无明文）；`pnpm smoke:core` 6/6；`pnpm smoke:m1` 23/23；`pnpm e2e:m1` 19/19；`pnpm lint` / `pnpm typecheck` 零问题；`pnpm --filter @ai-ide/desktop build` 成功；`pnpm types:gen` 无漂移。CI 定义已含上述步骤（含日志密钥扫描步骤，跑的是 `node scripts/scan-log-secrets.mjs --probe`），可用 `pnpm verify:ci` 本地等价复现（**远端 Actions 尚未真正执行**）；M5 专项：`pnpm verify:m5`。

### 质量门禁实测（2026-10-25，LSP 收口）

前端门禁本轮实跑：`pnpm typecheck`（`tsc --noEmit`）退出码 0；`pnpm test` **27 文件 500 项全过**，其中 LSP 相关 `lsp-providers.test.ts` 16 项（坐标换算、四 provider 的返回翻译、重复注册幂等、卸载统一 dispose）、`lsp-store.test.ts` 23 项、`command-registry.test.ts` 25 项、`command-palette-store.test.ts` 15 项。内核侧本轮未改动 Rust 代码，`cargo test --workspace` 沿用插件 P5 收口时的 **1128 passed / 0 failed / 9 ignored**（LSP 流水线测试 `crates/core/tests/lsp_flow.rs` 在 `e03ed64` 中已通过，`crates/protocol` 的 `IMPLEMENTED.len() == 79` 断言同轮生效）。

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
