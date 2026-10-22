---
AIGC:
    Label: "1"
    ContentProducer: 001191440300708461136T1XGW3
    ProduceID: 04526fdd866a8c5c9aceee6b83ed2c05_c3c28b8ecb5d11f18e0e52540064ee0f
    ReservedCode1: iMQU+M3l+Nemh2SBP+wNxuo62CntVD5M1UZnuDR9sSoknT6QsOyMHgm1pJx0WXt/bNlkb7FNjDO3LmUPWOWaPFJp3mYYmX7Y9sCruOUtqG+Mayg6JJ/qBC9B5C67FLWMVCVrecWvIl1J8s/wY7Op/4zr7wuXFfFQmhoRhDMH4sWfznzSnZn+53A1YFY=
    ContentPropagator: 001191440300708461136T1XGW3
    PropagateID: 04526fdd866a8c5c9aceee6b83ed2c05_c3c28b8ecb5d11f18e0e52540064ee0f
    ReservedCode2: iMQU+M3l+Nemh2SBP+wNxuo62CntVD5M1UZnuDR9sSoknT6QsOyMHgm1pJx0WXt/bNlkb7FNjDO3LmUPWOWaPFJp3mYYmX7Y9sCruOUtqG+Mayg6JJ/qBC9B5C67FLWMVCVrecWvIl1J8s/wY7Op/4zr7wuXFfFQmhoRhDMH4sWfznzSnZn+53A1YFY=
---

# AI IDE

本地优先、模型可插拔的 AI 原生代码编辑器。

外壳为 **Electron + React + TypeScript**，内核为 **Rust**，两者通过 **JSON-RPC 2.0 over stdio（NDJSON）** 通信。
设计文档见 [`docs/`](./docs/00-README.md)。

---

## 当前状态

| 里程碑 | 主题 | 状态 |
|---|---|---|
| M0 | 骨架与通信 | 已完成，验收见 [`docs/06-M0-验收报告.md`](./docs/06-M0-验收报告.md) |
| M1 | 编辑器壳 | 已完成，验收见 [`docs/07-M1-验收报告.md`](./docs/07-M1-验收报告.md) |
| M2 | 模型接入 | 已完成（内核 + 外壳），验收见 [`docs/08-M2-验收报告.md`](./docs/08-M2-验收报告.md) |
| M3 | 上下文引用 | 已完成，验收见 [`docs/09-M3-验收报告.md`](./docs/09-M3-验收报告.md) |
| M4 | Agent 闭环 | 内核与外壳主线已完成（Agent 循环与 8 个工具、权限闸门四动作、`agent.*` 协议与 core 接线、外壳运行面板与审批 UI），验收见 [`docs/10-M4-验收报告.md`](./docs/10-M4-验收报告.md)；端到端基线任务与真实上游实跑已完成（基线 4/4、`edit_file` 非唯一匹配 20/20），联调凭据默认读仓库根 `.e2e-m4-credentials.json`（已在 `.gitignore` 中忽略） |
| M5 | 持久化与治理 | 已完成（内核 + 外壳主线 + 收口）：`crates/store` 会话 / 消息 / 审计仓储（事务内分叉、审计脱敏留存）、九个 `session.*` 与只读 `audit.list`、检查点回滚（`checkpoint.create` / `list` / `restore` / `delete`，快照 SHA-256 逐文件复核）、JSONL 结构化日志（按天滚动 + 脱敏开关 + 32 MiB 滚动上限）、权限表持久化（`permission.list` / `revoke`，落 `permission_rule` 表），能力清单 30 → 46，验收见 [`docs/11-M5-验收报告.md`](./docs/11-M5-验收报告.md) |
| M6 | 终端 + Git | **切片一（内核终端）已完成**：`crates/terminal`（`portable-pty` 之上的 `TerminalManager`，多会话 / resize 夹取 / 退出清理 / 单一事件通道）、`terminal.create` / `write` / `resize` / `kill` / `list` 五方法与 `terminal.data` / `terminal.exit` 两事件已进协议 `IMPLEMENTED` 并挂进 `crates/core` dispatch（`terminal_api.rs`），能力清单 46 → **51**；测试：`crates/terminal` 单测 13 项 + `crates/core/tests/terminal_flow.rs` 端到端 8 项（能力清单、多会话不串台、resize 夹取、shell 自行退出与 kill 两条清理路径、base64 / 启动失败错误码）。Git（`git.*`）与外壳终端面板未做 |
| M7–M8 | MCP / 打包 | 未开工；M8 仅打包链路先行落地（产出 `AI-IDE-0.1.0-win-x64.exe`，core 二进制在 asar 外），干净环境安装验收未做 |

**已可运行的能力**：打开工作区与文件树（懒加载、忽略规则）、Monaco 多标签编辑与保存（乐观锁 hash + CRLF 往返还原）、文件名模糊检索与内容检索、外部改动监听与通知、Main 侧策略闸门、内核进程监管与优雅退出、Rust 协议类型自动生成到 TS、Provider / 模型目录（落盘 `providers.json`、密钥加密存储）、OpenAI 兼容流式对话（`llm.chat_stream` / `cancel` / `list_models` / `count_tokens`，含 `llm.error` 错误分类）、Agent 闭环（8 个工具、权限闸门四动作、diff 预览与审批）、会话持久化（首条消息自动建会话、流结束按用量落 `assistant`，杀进程重启后历史完整）、会话导出（Markdown / JSON，落点只经系统保存对话框）、会话分叉、只读审计（会话创建 / 更新 / 删除 / 导出 / 分叉留痕，存储层脱敏）、检查点与回滚（手动 / Agent 自动留痕，回滚后逐文件 SHA-256 复核）、JSONL 结构化日志（`<data_dir>/logs/core-YYYY-MM-DD.jsonl`，按天滚动、出口脱敏）、权限表持久化（`allow_always` 落库，重启后仍生效，可在面板撤销）、内核终端会话（`terminal.create` / `write` / `resize` / `kill` / `list`，PTY 原始字节经 `terminal.data` 推送，子进程退出（含自行 `exit`）统一走 `terminal.exit` 并自动从列表摘除）。

**尚未实现**：终端面板（内核能力已就绪、外壳未接）、Git UI、MCP、安装包的干净环境验收、Inline Edit（Ctrl+K）、Tab 补全、LSP、会话与审计列表的「加载更多」、消息内容检索与消息级分叉入口；M4 遗留：工具调用独立卡片组件、多模型复测（`scripts/e2e-m4.mjs` 已落地，跑 `pnpm e2e:m4` 即可，凭据默认读 `<仓库根>/.e2e-m4-credentials.json`，该文件已在 `.gitignore` 中忽略、也可用 `--creds` 覆盖）。

---

## 技术栈

| 层 | 选型 | 版本基线 |
|---|---|---|
| 运行时 | Node.js | >= 20.11 |
| 包管理 | pnpm workspace | pnpm 12 |
| 壳 | Electron | ^44 |
| 构建 | electron-vite + Vite | ^6 / ^8 |
| UI | React + TypeScript | ^19 / ^5.6 |
| 编辑器 | Monaco Editor | ^0.57 |
| 前端测试 | Vitest | ^3 |
| 端到端 | Playwright（驱动 Electron） | playwright-core ^1.64 |
| 代码检查 | ESLint（flat config）+ typescript-eslint | ^10 / ^8 |
| 内核 | Rust（edition 2021，rust-version 1.79） | stable |
| 异步运行时 | tokio | 1.x |
| 协议 | JSON-RPC 2.0 over stdio（NDJSON） | v1.0 |

---

## 目录结构

```
ai_ide/
├─ apps/
│  └─ desktop/                  # Electron 应用（main / preload / renderer）
├─ crates/
│  ├─ core/                     # 内核可执行文件 ai-ide-core：分发器与生命周期
│  ├─ protocol/                 # JSON-RPC 帧、DTO、方法名常量（契约唯一来源）
│  ├─ workspace/                # 工作区注册表、文件树、文件读写、监听、路径守卫
│  ├─ indexer/                  # 文件名模糊检索与内容检索
│  ├─ llm/                      # Provider 归一化、SSE 解析、密钥加密、token 计数
│  ├─ agent/                    # Agent 循环、工具、权限、diff
│  └─ store/                    # SQLite 仓储：会话 / 消息 / 审计（sqlx 迁移）
├─ packages/
│  └─ shared-types/             # TS 侧类型：src/generated/（ts-rs 产出，禁手改）+ 运行时常量镜像
├─ scripts/                     # 类型生成、协议冒烟、内核功能冒烟、E2E、基准
├─ docs/                        # 设计文档与验收报告
├─ .github/workflows/ci.yml     # CI 定义
└─ package.json / pnpm-workspace.yaml / Cargo.toml
```

---

## 快速开始

前置：Node.js >= 20.11、pnpm、Rust stable 工具链（`rustc >= 1.79`）。

```powershell
pnpm install
cargo build -p ai-ide-core
pnpm dev
```

`pnpm dev` 会以 electron-vite 开发模式拉起外壳，并由主进程自动启动 Rust 内核子进程。
（`pnpm install` 会通过 `.npmrc` 中的 `electron_mirror` 走国内镜像下载 Electron 二进制。）

---

## 常用命令

| 命令 | 作用 |
|---|---|
| `pnpm dev` | 开发模式启动外壳（自动拉起内核） |
| `pnpm build` | 构建外壳（electron-vite build） |
| `pnpm lint` | ESLint 全量检查 |
| `pnpm typecheck` | TS 类型检查（所有 workspace 包） |
| `pnpm test` | 前端单元测试（vitest） |
| `cargo test --workspace` | Rust 单元 / 集成测试 |
| `cargo clippy --workspace --all-targets -- -D warnings` | Rust 静态检查（零告警） |
| `cargo fmt --all -- --check` | Rust 格式检查 |
| `pnpm types:gen` | 由 Rust 重新生成 TS 协议类型（产物入库） |
| `pnpm smoke:core` | 协议冒烟（握手 / ping / 错误路径 / 优雅退出） |
| `pnpm smoke:m1` | 内核侧 M1 功能面冒烟 |
| `pnpm scan:secrets` | 日志密钥扫描门禁：检测器自检 + 拉起内核（`AI_IDE_LOG_PROBE=1`）写真实日志再扫，扫不到日志行也判失败 |
| `pnpm scan:secrets:self` | 只跑检测器自检（正样本必中、脱敏形态必放行） |
| `pnpm e2e:m1` | Playwright + Electron 真界面端到端 |
| `pnpm e2e:m4` | 真实模型 Agent 端到端联调（需本机凭据，不入 CI）；凭据默认读 `<仓库根>/.e2e-m4-credentials.json`（已在 `.gitignore` 中忽略），`--check-creds` 只做凭据自检 |
| `pnpm bench:m1` | 树列举 / 读文件 / 搜索耗时基准 |
| `pnpm verify:m0` / `pnpm verify:m1` / `pnpm verify:m2` / `pnpm verify:m3` / `pnpm verify:m4` / `pnpm verify:m5` | 对应里程碑的一键复跑（`verify:m3` / `verify:m4` 分别加跑 `cargo test -p ai-ide-llm` / `-p ai-ide-agent`；均不含需真实凭据的 `e2e:m4`） |
| `pnpm verify:ci` | 本地等价复现 CI 全流程 |

---

## 质量门禁

CI 定义在 [`.github/workflows/ci.yml`](./.github/workflows/ci.yml)，在 `windows-latest` 上执行两个 job：

- **rust**：`cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、构建 `ai-ide-core`、协议冒烟、日志密钥扫描（`node scripts/scan-log-secrets.mjs --probe`，本机等价入口 `pnpm scan:secrets`：先自检检测器，再由内核 `AI_IDE_LOG_PROBE=1` 写一批含假密钥的真实日志后扫描，取样为空即失败）。
- **desktop**：`pnpm install --frozen-lockfile`、`pnpm types:gen`、协议类型漂移检查（`git diff --exit-code`）、`pnpm lint`、`pnpm typecheck`、`pnpm test`、构建外壳。

本地以一条命令等价复现全部 CI 步骤：

```powershell
pnpm verify:ci
```

> 说明：远端已有 8 次 Actions 运行记录（截至 2026-10-21），但两个 job 均在启动前失败、0 步执行，CI 尚未真正跑过任何一步；当前以本地等价复跑作为替代验证手段。

---

## 已知限制

1. M8 已能产出安装包（`AI-IDE-0.1.0-win-x64.exe`），但尚未在干净 Windows 环境验证安装、运行与卸载。
2. CI 定义已就位，但远端 Actions 从未真正执行（两个 job 均在启动前失败、0 步执行），门禁尚未在远端验证。
3. renderer 首次构建体积较大（Monaco 全量 + 按需语言包），尚未做语言子集裁剪与 worker 异步分块。
4. `core.log` 通知通道已定义，内核侧尚未主动推送；UI 显示的是子进程 stderr 原始流。
5. ts-rs 在编译期会输出若干 `failed to parse serde attribute` 宏诊断，不影响生成结果。
6. 索引仅为内存态（文件名 + 内容），无持久化索引库；符号检索（`search.symbols`）未实现。
7. 检查点回滚已实现（`checkpoint.*` 四方法接进 dispatch，回滚后逐文件重算 SHA-256 并折成 `verified` 返回），边界如下：快照只覆盖工作区内经 `PathGuard` 与忽略规则筛选后的非敏感文件，单文件超 8 MiB、单检查点超 5000 文件 / 256 MiB 的条目跳过并计数；回滚只动快照登记过的文件，不删除清单外的新文件；全量快照无增量与去重；检查点无保留期与自动清理，需自行清理 `<data_dir>/checkpoints`。
8. 会话侧限制：列表固定拉 50 条、审计固定 20 条且无「加载更多」；标题过滤不检索消息内容；分叉只有「会话末尾」入口（内核支持 `from_message_id`，UI 未接）；消息落盘失败不重试，仅提示一次。
9. JSONL 日志只做「按天滚动 + 单文件 32 MiB 上限」，没有保留期与自动清理，长期运行需自行清理 `<data_dir>/logs`；`AI_IDE_LOG_REDACT=off` 只用于本地排查（脱敏默认开，CI 门禁按默认跑）。

---

## 文档

| 文档 | 内容 |
|---|---|
| [`docs/00-README.md`](./docs/00-README.md) | 文档索引与实现状态 |
| [`docs/01-需求规格说明.md`](./docs/01-需求规格说明.md) | 需求、优先级、MVP 边界 |
| [`docs/02-系统架构设计.md`](./docs/02-系统架构设计.md) | 三层架构、进程模型、模块划分、ADR、安全模型 |
| [`docs/03-内核接口协议规范.md`](./docs/03-内核接口协议规范.md) | 协议全文与方法目录 |
| [`docs/04-AI能力与Agent设计.md`](./docs/04-AI能力与Agent设计.md) | Provider 抽象、上下文组装、Agent 循环、工具与权限 |
| [`docs/05-里程碑与验证计划.md`](./docs/05-里程碑与验证计划.md) | M0–M8 计划、测试策略、风险、待决问题 |
| [`docs/06-M0-验收报告.md`](./docs/06-M0-验收报告.md) / [`docs/07-M1-验收报告.md`](./docs/07-M1-验收报告.md) | 里程碑验收实测记录（M0 / M1） |
| [`docs/08-M2-验收报告.md`](./docs/08-M2-验收报告.md) / [`docs/09-M3-验收报告.md`](./docs/09-M3-验收报告.md) | 里程碑验收实测记录（M2 / M3） |
| [`docs/10-M4-验收报告.md`](./docs/10-M4-验收报告.md) | M4 实测验收：Agent 闭环、权限闸门、真实模型端到端联调与遗留项 |
| [`docs/11-M5-验收报告.md`](./docs/11-M5-验收报告.md) | M5 实测验收：会话持久化、导出与分叉、审计与脱敏、检查点回滚、JSONL 日志与密钥扫描门禁、权限表持久化、外壳会话面板、验收标准对照与遗留项 |

---

## 许可

MIT，见 [LICENSE](./LICENSE)。
*（内容由AI生成，仅供参考）*
