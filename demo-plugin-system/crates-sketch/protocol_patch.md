# 内核接线补丁清单（P0 施工用）

本文件是「改哪几个文件、加哪几行」的清单，不是代码。原则只有一条：
**方法名进表与 dispatch 分支必须同一次提交完成**，否则 `core.hello` 会上报一个
调不通的方法——这违反「能力不许虚报」不变量，而且是最难排查的一类问题。

---

## 1. `protocol/src/methods.rs`

新增 10 个常量，插在 `plugin` 分组的首尾之间，保持按域分组：

```rust
pub mod plugin {
    pub const LIST: &str    = "plugin.list";
    pub const STATUS: &str  = "plugin.status";
    pub const INSTALL: &str = "plugin.install";
    pub const UNINSTALL: &str = "plugin.uninstall";
    pub const LOAD: &str    = "plugin.load";
    pub const UNLOAD: &str  = "plugin.unload";
    pub const RELOAD: &str  = "plugin.reload";
    pub const CALL: &str    = "plugin.call";
    pub const GRANT: &str   = "plugin.grant";
    pub const REVOKE: &str  = "plugin.revoke";
}
```

`IMPLEMENTED` 追加同样 10 项。**不要**把插件内部的 `tool/invoke`、
`host/fs.read` 加进去——它们不是内核协议的一部分，加进去等于承认插件方法可以
随意膨胀全局方法表。

## 2. `protocol/src/error.rs`

6xxx 段此前为空，整段划给插件：

```rust
pub const PLUGIN_NOT_FOUND: i32        = 6001;
pub const PLUGIN_CRASHED: i32          = 6002;
pub const CAPABILITY_DENIED: i32       = 6003;
pub const PLUGIN_API_MISMATCH: i32     = 6004;
pub const PLUGIN_TIMEOUT: i32          = 6005;
pub const PLUGIN_PROTOCOL_VIOLATION: i32 = 6006;
pub const PLUGIN_MANIFEST_INVALID: i32 = 6007;
pub const TOOL_CONFLICT: i32           = 6008;
```

同时补 `is_plugin_error(code)` 供 Main 侧区分「该弹插件诊断面板」还是「该弹普通错误」。

## 3. `core/src/dispatch.rs`

现有 match 是穷举的，加 10 个分支即可，**不做任何动态派发**：

```rust
methods::plugin::LIST      => host.plugin_list().await.map(Value::from),
methods::plugin::STATUS    => host.plugin_status(&p.plugin_id).await.map(Value::from),
methods::plugin::INSTALL   => host.plugin_install(&p.path, &p.capabilities).await?,
methods::plugin::UNINSTALL => host.plugin_uninstall(&p.plugin_id).await?,
methods::plugin::LOAD      => host.plugin_load(&p.plugin_id).await?,
methods::plugin::UNLOAD    => host.plugin_unload(&p.plugin_id).await?,
methods::plugin::RELOAD    => host.plugin_reload(&p.plugin_id).await?,
methods::plugin::CALL      => host.plugin_call(&p.plugin_id, &p.method, p.params).await,
methods::plugin::GRANT     => host.plugin_grant(&p.plugin_id, p.capabilities).await?,
methods::plugin::REVOKE    => host.plugin_revoke(&p.plugin_id, p.capabilities).await?,
```

`plugin.call` 的 params 必须**先校验再转发**：

```rust
#[derive(Deserialize)]
struct PluginCallParams {
    plugin_id: String,
    method: String,
    #[serde(default)]
    params: Value,
}
```

## 4. `core/src/state.rs`

`AppState` 增加一个字段，用 `Arc` 共享，生命周期与 Core 进程一致：

```rust
pub plugins: Arc<PluginHost>,
```

在 `AppState::new` 处构造，并在 `shutdown` 分支调用 `plugins.shutdown().await`——
漏了这一步，Core 退出时插件子进程会变成孤儿（Windows 上尤其明显）。

## 5. `core/src/tools/registry.rs`

不改结构，只加注册入口。`PluginTool` 已在 `tool_adapter.rs` 里实现 `Tool`。

```rust
// 插件加载成功后
for tool in plugin.tools() {
    registry.with_tool(Box::new(PluginTool::new(...)))?;  // 冲突返回 6008，不 panic
}
```

`with_tool` 的重复检查在这里正好复用：核心工具与 `plugin.` 前缀天然不撞，
撞了就是插件 id 重复或工具名重复，属于真错误。

## 6. `main/src/policy.ts` + `preload`

调用白名单追加 10 个方法名。**注意**：`plugin.call` 是唯一能拿到任意插件能力的入口，
在 policy 层要额外校验 `plugin_id` 是否在已加载集合内——policy 不该假设 Core 已经校验过，
两道防线不冗余，因为 policy 拦的是 Renderer 侧的越权渲染进程。

## 7. `shared-types`

由 protocol 生成后追加 `PluginSummary` / `PluginState` / `PluginToolSummary` 三个类型，
Renderer 只读这三个，不直接接触 manifest 原文（manifest 里可能有作者填的自由文本，
直接渲染要防注入）。

---

## 落地顺序（每一步都可独立回滚）

| 步 | 动作 | 回滚方式 |
|---|---|---|
| 1 | protocol 加常量 + 错误码，dispatch 加返回 6001 的空分支 | 删分支 |
| 2 | 实现 `plugin.list/status`（纯内存态） | 无副作用，可直接关 |
| 3 | 实现 load/unload/reload + 进程监管 | 配置里禁用插件目录 |
| 4 | 接 `PluginTool` 进 ToolRegistry | 卸载全部插件即回到 v0 行为 |
| 5 | 接 approvals + 审计 | 关 feature flag 后插件工具退回 `read` 语义 |
| 6 | grant/revoke + UI | 无需回滚，纯增量 |

第 4 步之后插件就能被模型调用，所以**第 4 步必须与第 5 步同版本发布**：
先上工具、后上审批，中间那段时间插件的写操作是裸奔的。
