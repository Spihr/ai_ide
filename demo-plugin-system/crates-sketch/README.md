# 内核骨架说明（参考，未接线）

```
crates-sketch/
├── plugin_host.rs     插件宿主：状态机、装载/握手、plugin.call 网关、host/* 代理
├── tool_adapter.rs    PluginTool: impl Tool —— 让插件工具进 ToolRegistry
└── protocol_patch.md  改哪几个文件、加哪几行（P0 施工清单）
```

## 这份骨架的定位

- **不是可编译的 crate**：没有 `Cargo.toml`，也没有进 `core/Cargo.toml` 的依赖。
  直接拷进仓库编译一定会报错——路径（`crate::workspace::PathGuard` 等）按
  现有目录结构写的，但 `unimplemented!()` 与占位 `PluginError` 是有意留的。
- **目的是锁死三件容易走偏的事**：
  1. 闸门顺序（状态 → 归属 → 能力 → 路径 → 审批 → 资源）不能交换；
  2. 插件方法不进 `methods::IMPLEMENTED`；
  3. 握手要双向校验，多报和少报都拒载。
- 真正的行为验证在 `../mock-host` 里，同一套状态机与闸门顺序已有 Node 可跑版本，
  `node run-demo.mjs` 12/12 通过。

## 与 Node demo 的对应关系

| Node（demo） | Rust（骨架） |
|---|---|
| `mock-host/host.mjs` | `plugin_host.rs` |
| `mock-host/supervisor.mjs` | `PluginProcess`（未实现） |
| `mock-host/manifest.mjs` | `Manifest` + 校验（未实现） |
| `mock-host/path-guard.mjs` | 复用 `crates/workspace` 的 `PathGuard` |
| `mock-host/permission.mjs` | 复用 `PermissionGate` |
| `tool_adapter.rs` 的 Node 等价物 | `host.mjs` 里的 `toolIndex` + 审批映射 |

## 先做哪一步

按 `protocol_patch.md` 末尾的表走。第 1 步（方法名 + 空分支）可以独立提交，
第 4 步与第 5 步（工具注册 + 审批）必须同版本发布，不能拆。
