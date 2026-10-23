# demo-plugin-system

插件系统的可运行验证原型：**子进程 + stdio NDJSON**，零第三方依赖，`node run-demo.mjs` 即可。

配套设计文档：[`../docs/12-插件系统设计.md`](../docs/12-插件系统设计.md)

```bash
cd demo-plugin-system
node run-demo.mjs
```

要求 Node ≥ 18（用到 `import`、`readline` 与 `fs.rmSync`）。无需 `npm install`。

## 它验证了什么

12 个场景，全部断言错误码与副作用，而不是「看起来跑通了」：

| # | 场景 | 断言点 |
|---|---|---|
| 01 | 非法清单静态拒载 | id / 版本 / pluginApi / main 逃逸 / 未知能力 / risk 与能力矛盾，6 条错误一次报全 |
| 02 | 未授权加载 | 返回 6003 且**没有 spawn 进程** |
| 03 | 授权后加载 | 握手成功后注册 6 个 `plugin.demo-hello.*` 工具 |
| 04 | 只读工具 | 不触发审批，结果回灌 |
| 05 | 写工具 | 审批通过 → 落盘 → 校验字节数 |
| 06 | 审批拒绝 | 返回 3001 且文件**没有**被创建 |
| 07 | 路径闸门 | `..` → 1002，`.git/config` → 1003，绝对路径 → 1002 |
| 08 | 能力闸门 | 调未申请的 `net.fetch` → 6003 |
| 09 | 资源配额 | 第 201 次宿主调用 → 6009 |
| 10 | 事件广播 | 只投给声明 `subscriptions` 的插件 |
| 11 | 崩溃恢复 | 进程退出 → 工具立即下架 → 调用返回 6002 → 重启后恢复 |
| 12 | 卸载 | 工具清零，进程退出 |

## 目录

```
demo-plugin-system/
├── run-demo.mjs              11 个场景的编排器
├── mock-host/
│   ├── protocol.mjs          帧编解码、方法常量、错误码、上限
│   ├── manifest.mjs          清单校验（形状 → 语义 → 与能力交叉）
│   ├── path-guard.mjs        路径闸门（相对路径 only / .. / 敏感段）
│   ├── permission.mjs        能力授权表 + 审批队列（scope 到文件）
│   ├── supervisor.mjs        子进程监管（帧上限 / 超时 / 崩溃在途请求失败）
│   └── host.mjs              PluginHost：状态机、握手、调用网关、host/* 代理、审计
├── plugins/demo-hello/
│   ├── plugin.json           清单：3 项能力 + 6 个工具（含 3 个闸门探针）
│   └── index.mjs             插件侧 RPC 实现（stdout 只写协议，日志走 stderr）
├── crates-sketch/            内核骨架参考（不接线，见其 README）
└── fixtures/workspace/       示例工作区（`.git/config` 由 run-demo 生成，不入库）
```

运行时会生成 `.tmp-workspace/`（fixtures 的副本），写工具的结果落在
`.tmp-workspace/docs/plugin-note.md`。

## 两个关键实现细节

**stdout 是协议专用通道。** 插件里任何 `console.log` 都会污染帧流，
宿主侧会直接解析失败并判 6006。所以 `index.mjs` 只有 `log()` 一个出口，写 stderr，
宿主把它转成 `plugin.log` 事件。

**握手是双向校验。** 插件多报工具（清单没写但实现了）= 运行时偷偷扩权，
少报（清单写了没实现）= 清单骗人，两者都判 6006 拒载。工具定义一律以清单为准。

## 未覆盖的部分

- 不做 UI 贡献点（面板/状态栏），v1 只做「Agent 工具 + 用户命令」两类扩展。
- 不做签名校验与市场分发，`plugin.install` 只接受本地目录。
- `host/llm.chat`、`host/net.fetch` 只有闸门，没有真实实现——它们验的是权限而非功能。
