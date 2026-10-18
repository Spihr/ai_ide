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

> 状态：设计阶段（v0.1，待审阅）· 尚未开始编码

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
