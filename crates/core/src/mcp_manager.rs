//! MCP 连接管理器（M7 切片二）。
//!
//! `crates/mcp` 只回答「怎么连一个 Server、怎么列工具、怎么调工具」；本模块回答
//! 「当前进程连着哪些 Server、各自发现到什么工具、哪些工具该被注入 agent」。
//!
//! 两条硬约束：
//!
//! 1. **多 Server 并存**：一个 Server 一条独立连接，互不影响。某个 Server 挂了，
//!    不该把别的 Server 的工具一起拖下水——所以这里是 `name → Link` 的表，
//!    不是「一个当前连接」。
//! 2. **配置与连接分离**：`mcp.json` 里写着 ≠ 已经连上。`mcp.servers` 必须同时
//!    报出这两面（`configured` / `connected`），外壳才能画出「连接 / 断开」按钮。
//!
//! 同步/异步边界：连接与调用都是异步的（子进程管道、HTTP），而 agent 的工具接口
//! 是同步的。桥接只发生在 [`McpManager::call_tool_blocking`] 一处，见那里的注释。

use ai_ide_mcp::{
    tool_namespace, CallToolResult, ClientState, McpClient, McpConfig, McpError, McpTool,
};
use ai_ide_protocol::dto::{McpContentBlock, McpServerInfo, McpToolInfo};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

/// 已连接的一个 Server。
struct Link {
    /// 连接句柄。`Arc` 是为了让同步工具桥能把它挪进独立的调用线程。
    client: Arc<McpClient>,
    /// 最近一次 `tools/list` 的结果（顺序按 Server 返回）。
    tools: Vec<McpTool>,
}

/// 一次 `mcp.connect` 的结果。
#[derive(Debug, Clone)]
pub struct ConnectedServer {
    /// Server 名。
    pub server: String,
    /// 传输类型（`stdio` / `http` / `sse`）。
    pub transport: String,
    /// 协商出的协议版本。
    pub protocol_version: Option<String>,
    /// 发现到的工具（已带命名空间）。
    pub tools: Vec<McpToolInfo>,
}

/// MCP 连接管理器。
pub struct McpManager {
    /// 启动时读到的配置。运行期不重读：改 `mcp.json` 要重启内核才生效，
    /// 这与「配置是用户的显式声明」一致——静默热加载会让已连的连接与配置打架。
    config: McpConfig,
    /// Server 名 → 连接。
    links: RwLock<BTreeMap<String, Link>>,
    /// Server 名 → 最近一次失败原因（成功连接后清掉）。
    last_errors: RwLock<BTreeMap<String, String>>,
}

impl McpManager {
    /// 用一份配置构造（空配置也合法：此时 `mcp.servers` 回空表）。
    pub fn new(config: McpConfig) -> Self {
        Self {
            config,
            links: RwLock::new(BTreeMap::new()),
            last_errors: RwLock::new(BTreeMap::new()),
        }
    }

    /// 读默认位置（`%APPDATA%\ai-ide\mcp.json`，可用 `AI_IDE_MCP_CONFIG` 覆盖）。
    ///
    /// 读失败不致命：配置坏了按空配置起，`mcp.servers` 会回空表——总比整个内核
    /// 起不来好。错误进日志，便于诊断包定位。
    pub fn from_default_config() -> Self {
        match McpConfig::load_default() {
            Ok(config) => {
                tracing::debug!(
                    target: "mcp",
                    servers = config.servers.len(),
                    "已装载 MCP 配置"
                );
                Self::new(config)
            }
            Err(err) => {
                tracing::warn!(target: "mcp", error = %err, "MCP 配置装载失败，按空配置起");
                Self::new(McpConfig::default())
            }
        }
    }

    /// 配置里的 Server 名（升序）。
    pub fn configured_names(&self) -> Vec<String> {
        self.config.servers.keys().cloned().collect()
    }

    /// 该名字是否在配置里。
    pub fn is_configured(&self, name: &str) -> bool {
        self.config.server(name).is_some()
    }

    /// 已连接的 Server 数量。
    pub fn link_count(&self) -> usize {
        self.links.read().map(|links| links.len()).unwrap_or(0)
    }

    /// 全部 Server 的运行期视图：配置里有 ∪ 连接里有的，按名字升序。
    pub fn server_infos(&self) -> Vec<McpServerInfo> {
        let links = match self.links.read() {
            Ok(links) => links,
            Err(poisoned) => poisoned.into_inner(),
        };
        let errors = match self.last_errors.read() {
            Ok(errors) => errors,
            Err(poisoned) => poisoned.into_inner(),
        };
        let names: BTreeSet<String> = self
            .config
            .servers
            .keys()
            .cloned()
            .chain(links.keys().cloned())
            .collect();
        names
            .into_iter()
            .map(|name| {
                let configured = self.config.server(&name).is_some();
                let link = links.get(&name);
                McpServerInfo {
                    transport: self
                        .config
                        .server(&name)
                        .map(|config| config.transport_label().to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    configured,
                    connected: link
                        .map(|link| link.client.state() == ClientState::Ready)
                        .unwrap_or(false),
                    state: link
                        .map(|link| state_label(link.client.state()).to_string())
                        .unwrap_or_else(|| "configured".to_string()),
                    tool_count: link.map(|link| link.tools.len() as u32).unwrap_or(0),
                    last_error: errors.get(&name).cloned(),
                    name,
                }
            })
            .collect()
    }

    /// 连接一个 Server 并把它的工具拉下来。
    ///
    /// 幂等：已处于 `Ready` 的 Server 直接回缓存，不重复握手（重复握手会起第二个
    /// 子进程，白白多一份开销）。已失效（`Stale` / `Closed`）的连接先关掉再重连。
    pub async fn connect(&self, name: &str) -> Result<ConnectedServer, McpError> {
        let config = self
            .config
            .server(name)
            .cloned()
            .ok_or_else(|| McpError::UnknownServer(name.to_string()))?;

        if let Some(existing) = self.ready_link(name) {
            return Ok(ConnectedServer {
                server: name.to_string(),
                transport: config.transport_label().to_string(),
                protocol_version: existing.client.protocol_version(),
                tools: info_of(name, &existing.tools),
            });
        }

        // 旧连接（未就绪）必须先关：不关就重连，旧子进程会留在后台。
        if let Some(stale) = self.take_link(name) {
            stale.client.close().await;
        }

        let client = match McpClient::connect(&config).await {
            Ok(client) => client,
            Err(err) => {
                self.record_error(name, &err);
                return Err(err);
            }
        };
        let tools = match client.list_tools().await {
            Ok(tools) => tools,
            Err(err) => {
                // 连上了但列不出工具，这条连接对上层毫无用处：关掉，按失败上报。
                client.close().await;
                self.record_error(name, &err);
                return Err(err);
            }
        };

        let protocol_version = client.protocol_version();
        let infos = info_of(name, &tools);
        let client = Arc::new(client);
        if let Ok(mut links) = self.links.write() {
            links.insert(
                name.to_string(),
                Link {
                    client: Arc::clone(&client),
                    tools,
                },
            );
        }
        self.clear_error(name);
        Ok(ConnectedServer {
            server: name.to_string(),
            transport: config.transport_label().to_string(),
            protocol_version,
            tools: infos,
        })
    }

    /// 断开一个 Server，返回 `(本次是否真的关掉连接, 下架的工具数)`。
    ///
    /// 幂等：没连着就回 `(false, 0)`，不报错——UI 的断开按钮不该因为连点两下弹框。
    /// 名字既不在配置里、也没有连接，才是参数错误（[`McpError::UnknownServer`]）。
    pub async fn disconnect(&self, name: &str) -> Result<(bool, u32), McpError> {
        let link = self.take_link(name);
        if link.is_none() && !self.is_configured(name) {
            return Err(McpError::UnknownServer(name.to_string()));
        }
        let Some(link) = link else {
            return Ok((false, 0));
        };
        let removed = link.tools.len() as u32;
        link.client.close().await;
        Ok((true, removed))
    }

    /// 已连接 Server 的工具清单。
    ///
    /// `server` 为 `None` 时返回全部；给了名字但没连上则回空表（不是错误——
    /// 「没连上所以没工具」是正常的查询结果，调用方要报错请用 `mcp.call`）。
    pub fn tool_infos(&self, server: Option<&str>) -> Vec<McpToolInfo> {
        let links = match self.links.read() {
            Ok(links) => links,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut out = Vec::new();
        for (name, link) in links.iter() {
            if let Some(wanted) = server {
                if wanted != name {
                    continue;
                }
            }
            out.extend(info_of(name, &link.tools));
        }
        out.sort_by(|left, right| left.namespaced_name.cmp(&right.namespaced_name));
        out
    }

    /// 某个 Server 的工具定义（原始 [`McpTool`]，供调用前校验与透传 schema）。
    pub fn tool_of(&self, server: &str, tool: &str) -> Option<McpTool> {
        let links = match self.links.read() {
            Ok(links) => links,
            Err(poisoned) => poisoned.into_inner(),
        };
        links
            .get(server)?
            .tools
            .iter()
            .find(|candidate| candidate.name == tool)
            .cloned()
    }

    /// 取一条连接（未连接为 `None`）。
    pub fn client_of(&self, server: &str) -> Option<Arc<McpClient>> {
        let links = match self.links.read() {
            Ok(links) => links,
            Err(poisoned) => poisoned.into_inner(),
        };
        links.get(server).map(|link| Arc::clone(&link.client))
    }

    /// 调用一个工具（异步路径，给 `mcp.call` 用）。
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let client = self.require_client(server)?;
        let outcome = client.call_tool(tool, arguments).await;
        if let Err(err) = &outcome {
            // 调用失败记下来：`mcp.servers` 的 `last_error` 是外壳唯一能看到的现场。
            if !matches!(err, McpError::Rpc { .. }) {
                self.record_error(server, err);
            }
        }
        outcome
    }

    /// 同步调用一个工具，给 agent 的 [`ai_ide_agent::Tool::call`] 用。
    ///
    /// 为什么要单起一条线程：MCP 调用是异步的（子进程管道 / HTTP），而工具接口是
    /// 同步的。这里**不能**用 `Handle::block_on`——如果当前正跑在 tokio worker 上，
    /// 它会直接 panic；也不能 `block_in_place`，那要求 multi-thread runtime，
    /// 单测里的 `current_thread` 会炸。于是把调用挪到一条普通线程，它自带一个
    /// current-thread runtime，跑完把结果送回来，调用方只阻塞自己这一条线程。
    ///
    /// tokio 的连接句柄（mpsc / 子进程管道）跨 runtime 使用是安全的：内部状态不绑
    /// 定创建它的 reactor，只绑定「谁在 poll 它」。
    pub fn call_tool_blocking(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let client = self.require_client(server)?;
        let tool_owned = tool.to_string();
        let server_owned = server.to_string();
        let joined = std::thread::Builder::new()
            .name(format!("mcp-call-{server}"))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| McpError::Transport {
                        server: server_owned.clone(),
                        reason: format!("创建调用运行时失败：{err}"),
                    })?;
                runtime.block_on(client.call_tool(&tool_owned, arguments))
            })
            .map_err(|err| McpError::Transport {
                server: server.to_string(),
                reason: format!("创建调用线程失败：{err}"),
            })?;
        match joined.join() {
            Ok(result) => {
                if let Err(err) = &result {
                    if !matches!(err, McpError::Rpc { .. }) {
                        self.record_error(server, err);
                    }
                }
                result
            }
            // 线程 panic 说明 crate 里有 bug；按传输错误上报，别把进程带崩。
            Err(_) => Err(McpError::Transport {
                server: server.to_string(),
                reason: "MCP 调用线程异常退出".to_string(),
            }),
        }
    }

    /// 关闭全部连接（进程收工时调用）。逐个关，单个失败不影响其余。
    pub async fn shutdown_all(&self) {
        let links: Vec<Link> = match self.links.write() {
            Ok(mut links) => std::mem::take(&mut *links).into_values().collect(),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner())
                .into_values()
                .collect(),
        };
        for link in links {
            link.client.close().await;
        }
    }

    /// 取连接；没连上就按「配置里有 → 未连接」「配置里没有 → 未知 Server」分开报。
    fn require_client(&self, server: &str) -> Result<Arc<McpClient>, McpError> {
        if let Some(client) = self.client_of(server) {
            return Ok(client);
        }
        if self.is_configured(server) {
            Err(McpError::NotInitialized(format!(
                "MCP Server `{server}` 尚未连接，先调 mcp.connect"
            )))
        } else {
            Err(McpError::UnknownServer(server.to_string()))
        }
    }

    /// 已就绪的连接（克隆工具表，避免把锁带出函数）。
    fn ready_link(&self, server: &str) -> Option<ReadyLink> {
        let links = match self.links.read() {
            Ok(links) => links,
            Err(poisoned) => poisoned.into_inner(),
        };
        let link = links.get(server)?;
        if link.client.state() != ClientState::Ready {
            return None;
        }
        Some(ReadyLink {
            client: Arc::clone(&link.client),
            tools: link.tools.clone(),
        })
    }

    /// 摘掉一条连接（不关闭，由调用方决定怎么收尾）。
    fn take_link(&self, server: &str) -> Option<Link> {
        match self.links.write() {
            Ok(mut links) => links.remove(server),
            Err(poisoned) => poisoned.into_inner().remove(server),
        }
    }

    /// 记下失败原因。
    fn record_error(&self, server: &str, err: &McpError) {
        let message = err.to_string();
        match self.last_errors.write() {
            Ok(mut errors) => {
                errors.insert(server.to_string(), message);
            }
            Err(poisoned) => {
                poisoned.into_inner().insert(server.to_string(), message);
            }
        }
    }

    /// 清掉失败原因（连接成功后调用）。
    fn clear_error(&self, server: &str) {
        match self.last_errors.write() {
            Ok(mut errors) => {
                errors.remove(server);
            }
            Err(poisoned) => {
                poisoned.into_inner().remove(server);
            }
        }
    }

    /// 配置原文（只读快照，供测试断言）。
    pub fn config(&self) -> &McpConfig {
        &self.config
    }
}

/// 已就绪连接的快照。
struct ReadyLink {
    client: Arc<McpClient>,
    tools: Vec<McpTool>,
}

/// 把 Server 的工具表折成协议 DTO（带上命名空间）。
fn info_of(server: &str, tools: &[McpTool]) -> Vec<McpToolInfo> {
    let mut infos: Vec<McpToolInfo> = tools
        .iter()
        .map(|tool| McpToolInfo {
            name: tool.name.clone(),
            namespaced_name: tool_namespace(server, &tool.name),
            server: server.to_string(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        })
        .collect();
    infos.sort_by(|left, right| left.name.cmp(&right.name));
    infos
}

/// 连接状态标签，直接给外壳展示。
fn state_label(state: ClientState) -> &'static str {
    match state {
        ClientState::Connecting => "connecting",
        ClientState::Ready => "ready",
        ClientState::Stale => "stale",
        ClientState::Closed => "closed",
    }
}

/// 把 MCP 内容块折成协议 DTO。
///
/// 未知类型不丢：折成 `kind = "unknown"` 的空块，让前端知道「有东西但我们不认识」，
/// 而不是悄悄少一块内容。
pub fn content_blocks(blocks: &[ai_ide_mcp::McpContent]) -> Vec<McpContentBlock> {
    use ai_ide_mcp::McpContent;
    blocks
        .iter()
        .map(|block| match block {
            McpContent::Text { text } => McpContentBlock {
                kind: "text".into(),
                text: Some(text.clone()),
                ..McpContentBlock::default()
            },
            McpContent::Image { data, mime_type } => McpContentBlock {
                kind: "image".into(),
                data: Some(data.clone()),
                mime_type: Some(mime_type.clone()),
                ..McpContentBlock::default()
            },
            McpContent::Audio { data, mime_type } => McpContentBlock {
                kind: "audio".into(),
                data: Some(data.clone()),
                mime_type: Some(mime_type.clone()),
                ..McpContentBlock::default()
            },
            McpContent::Resource { resource } => McpContentBlock {
                kind: "resource".into(),
                resource: Some(resource.clone()),
                ..McpContentBlock::default()
            },
            McpContent::ResourceLink {
                uri,
                name,
                mime_type,
            } => McpContentBlock {
                kind: "resource_link".into(),
                uri: Some(uri.clone()),
                name: name.clone(),
                mime_type: mime_type.clone(),
                ..McpContentBlock::default()
            },
            McpContent::Unknown => McpContentBlock {
                kind: "unknown".into(),
                ..McpContentBlock::default()
            },
        })
        .collect()
}

/// 把调用结果折成「回灌给模型」的一段正文。
///
/// 优先用 crate 提供的 `text()`（文本块换行拼接）；文本为空时退回结构化输出的
/// JSON，再空就给一句明确的占位——不能让模型收到空字符串还以为工具成功了。
pub fn result_text(result: &CallToolResult) -> String {
    let text = result.text();
    if !text.trim().is_empty() {
        return text;
    }
    if let Some(structured) = &result.structured_content {
        return serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string());
    }
    if result.is_error {
        return "（MCP 工具执行失败，但没有返回错误文本）".to_string();
    }
    "（MCP 工具没有返回内容）".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_mcp::McpServerConfig;

    fn manager_with(servers: &[(&str, McpServerConfig)]) -> McpManager {
        let mut config = McpConfig::default();
        for (name, server) in servers {
            config.servers.insert((*name).to_string(), server.clone());
        }
        McpManager::new(config)
    }

    fn stdio_config(name: &str) -> McpServerConfig {
        McpServerConfig::Stdio(ai_ide_mcp::StdioServerConfig {
            name: name.to_string(),
            command: "definitely-not-a-real-binary".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        })
    }

    /// 没有配置也要能自处：`mcp.servers` 回空表，而不是报错。
    #[test]
    fn empty_config_reports_no_servers() {
        let manager = McpManager::new(McpConfig::default());
        assert!(manager.server_infos().is_empty());
        assert!(manager.tool_infos(None).is_empty());
        assert_eq!(manager.link_count(), 0);
        assert!(!manager.is_configured("anything"));
    }

    /// 配置里写着但没连：`configured = true` / `connected = false` / `state = configured`。
    #[test]
    fn configured_but_not_connected_is_visible() {
        let manager = manager_with(&[("fs", stdio_config("fs"))]);
        let infos = manager.server_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "fs");
        assert_eq!(infos[0].transport, "stdio");
        assert!(infos[0].configured);
        assert!(!infos[0].connected);
        assert_eq!(infos[0].state, "configured");
        assert_eq!(infos[0].tool_count, 0);
        assert!(infos[0].last_error.is_none());
    }

    /// 未连接就调用：配置里有 → `NotInitialized`（8003 那条路），没有 → `UnknownServer`。
    #[test]
    fn calling_before_connect_is_a_state_error() {
        let manager = manager_with(&[("fs", stdio_config("fs"))]);
        let err = manager
            .call_tool_blocking("fs", "read", serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(err, McpError::NotInitialized(_)), "{err}");

        let err = manager
            .call_tool_blocking("nope", "read", serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(err, McpError::UnknownServer(_)), "{err}");
    }

    /// 断开一个没连过的配置项是幂等的；断开一个完全不存在的名字才是错误。
    #[tokio::test]
    async fn disconnect_is_idempotent_but_rejects_unknown_names() {
        let manager = manager_with(&[("fs", stdio_config("fs"))]);
        assert_eq!(manager.disconnect("fs").await.expect("幂等"), (false, 0));
        assert!(matches!(
            manager.disconnect("nope").await.unwrap_err(),
            McpError::UnknownServer(_)
        ));
    }

    /// 未知 Server 的连接请求必须报 `UnknownServer`，且不能悄悄建空连接。
    #[tokio::test]
    async fn connect_rejects_unknown_server() {
        let manager = McpManager::new(McpConfig::default());
        assert!(matches!(
            manager.connect("nope").await.unwrap_err(),
            McpError::UnknownServer(_)
        ));
        assert_eq!(manager.link_count(), 0);
    }

    /// 文本块与结构化输出的折算是回灌模型的最后一道关：不能吐空串。
    #[test]
    fn result_text_never_returns_empty() {
        let text_only = CallToolResult {
            content: vec![ai_ide_mcp::McpContent::Text {
                text: "hello".into(),
            }],
            is_error: false,
            structured_content: None,
        };
        assert_eq!(result_text(&text_only), "hello");

        let structured = CallToolResult {
            content: Vec::new(),
            is_error: false,
            structured_content: Some(serde_json::json!({"ok": true})),
        };
        assert!(result_text(&structured).contains("\"ok\""));

        let empty = CallToolResult::default();
        assert!(!result_text(&empty).is_empty());
    }

    /// 内容块折算是「原样透传」：文本、图片、资源链接都得带过去，未知类型不丢。
    #[test]
    fn content_blocks_keep_every_kind() {
        let blocks = vec![
            ai_ide_mcp::McpContent::Text { text: "a".into() },
            ai_ide_mcp::McpContent::Image {
                data: "AAA".into(),
                mime_type: "image/png".into(),
            },
            ai_ide_mcp::McpContent::ResourceLink {
                uri: "file:///a.txt".into(),
                name: Some("a.txt".into()),
                mime_type: None,
            },
            ai_ide_mcp::McpContent::Unknown,
        ];
        let converted = content_blocks(&blocks);
        assert_eq!(converted.len(), 4);
        assert_eq!(converted[0].kind, "text");
        assert_eq!(converted[0].text.as_deref(), Some("a"));
        assert_eq!(converted[1].kind, "image");
        assert_eq!(converted[1].mime_type.as_deref(), Some("image/png"));
        assert_eq!(converted[2].kind, "resource_link");
        assert_eq!(converted[2].uri.as_deref(), Some("file:///a.txt"));
        assert_eq!(converted[3].kind, "unknown");
    }
}
