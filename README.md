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
| M4 | Agent 闭环 | 内核与外壳主线已完成（Agent 循环与 8 个工具、权限闸门四动作、`agent.*` 协议与 core 接线、外壳运行面板与审批 UI），验收见 [`docs/10-M4-验收报告.md`](./docs/10-M4-验收报告.md)；端到端脚本化基线尚缺 |
| M5–M8 | 持久化 / 终端与 Git / MCP / 打包 | 未开工 |

**已可运行的能力**：打开工作区与文件树（懒加载、忽略规则）、Monaco 多标签编辑与保存（乐观锁 hash + CRLF 往返还原）、文件名模糊检索与内容检索、外部改动监听与通知、Main 侧策略闸门、内核进程监管与优雅退出、Rust 协议类型自动生成到 TS、Provider / 模型目录（落盘 `providers.json`、密钥加密存储）、OpenAI 兼容流式对话（`llm.chat_stream` / `cancel` / `list_models` / `count_tokens`，含 `llm.error` 错误分类）。

**尚未实现**：会话持久化、内置终端、Git UI、MCP、安装包分发、Inline Edit（Ctrl+K）、Tab 补全、LSP；M4 遗留：端到端脚本化基线（`scripts/e2e-m4.mjs`）与真实上游模型的实跑验证。

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
│  └─ indexer/                  # 文件名模糊检索与内容检索
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
| `pnpm e2e:m1` | Playwright + Electron 真界面端到端 |
| `pnpm bench:m1` | 树列举 / 读文件 / 搜索耗时基准 |
| `pnpm verify:m0` / `pnpm verify:m1` | 对应里程碑的一键复跑 |
| `pnpm verify:ci` | 本地等价复现 CI 全流程 |

---

## 质量门禁

CI 定义在 [`.github/workflows/ci.yml`](./.github/workflows/ci.yml)，在 `windows-latest` 上执行两个 job：

- **rust**：`cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、构建 `ai-ide-core`、协议冒烟。
- **desktop**：`pnpm install --frozen-lockfile`、`pnpm types:gen`、协议类型漂移检查（`git diff --exit-code`）、`pnpm lint`、`pnpm typecheck`、`pnpm test`、构建外壳。

本地以一条命令等价复现全部 CI 步骤：

```powershell
pnpm verify:ci
```

> 说明：CI 尚未在仓库远端实际运行过（无 Actions 运行记录），当前以本地等价复跑作为替代验证手段。

---

## 已知限制

1. 未接入打包（electron-builder），只有开发态与 `electron-vite build` 产物，无安装包。
2. CI 定义已就位但未在远端实跑。
3. renderer 首次构建体积较大（Monaco 全量 + 按需语言包），尚未做语言子集裁剪与 worker 异步分块。
4. `core.log` 通知通道已定义，内核侧尚未主动推送；UI 显示的是子进程 stderr 原始流。
5. ts-rs 在编译期会输出若干 `failed to parse serde attribute` 宏诊断，不影响生成结果。
6. 索引仅为内存态（文件名 + 内容），无持久化索引库；符号检索（`search.symbols`）未实现。

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
| [`docs/06-M0-验收报告.md`](./docs/06-M0-验收报告.md) / [`docs/07-M1-验收报告.md`](./docs/07-M1-验收报告.md) | 里程碑验收实测记录 |

---

## 许可

MIT，见 [LICENSE](./LICENSE)。
*（内容由AI生成，仅供参考）*
