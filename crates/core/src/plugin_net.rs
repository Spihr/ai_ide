//! `host/net.fetch` 的出站网络闸门与传输层（插件系统 P5 切片五）。
//!
//! 这一层只做三件事，全部在**发起连接之前**完成：
//!
//! 1. **协议**：只放行 `http` / `https`（`file:` / `ftp:` / `data:` 一律拒）。
//! 2. **域名清单**：清单里 `allowedDomains` 没声明的域名，连 DNS 都不问一次。
//! 3. **地址**：解析出来的**每一个**地址都不许是回环 / 内网 / 链路本地，
//!    域名里直接写 IP 也一样过这道闸——否则「域名授权」会被
//!    `http://127.0.0.1` 这种写法一步绕过。
//!
//! 重定向不做「交给 HTTP 客户端自动跟」：自动跟等于把「跳去哪」的决定权交给
//! 服务端，白名单就形同虚设。这里关掉客户端的重定向策略，每一跳都回到本模块
//! 的 [`guard_url`] 重新过闸（协议 + 名单 + 地址），最多 [`MAX_REDIRECTS`] 跳。
//!
//! 传输实现走 [`NetTransport`]：生产是 [`ReqwestTransport`]（rustls），单测换成
//! 假的——这样「超时」「超限截断」「成功」这些分支不必真连网就能验。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use ai_ide_plugin::host_api::HostApiError;
use reqwest::Url;
use serde_json::{json, Value};

/// `host/net.fetch` 的默认响应体上限（256 KB）。
///
/// 和 `host/fs.read` 的 64 KB 不同量级：网页/接口响应带 HTML 骨架与 JSON 包裹，
/// 256 KB 才够一次「够用」的抽取；再大就该走分页参数，而不是把整站拉回来。
pub const DEFAULT_FETCH_BYTES: u64 = 256 * 1024;
/// 插件能申请到的最大响应体（2 MB）。上限是硬的：`maxBytes` 传再大也只到这里。
pub const MAX_FETCH_BYTES: u64 = 2 * 1024 * 1024;
/// 默认超时（10 s）。
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_millis(10_000);
/// 单次请求的最大超时（30 s）。超过就截到这里：宿主调用跑在插件的帧泵上，
/// 一条挂死的请求会把该插件的所有回包一起拖住。
pub const MAX_FETCH_TIMEOUT: Duration = Duration::from_millis(30_000);
/// 最小超时（200 ms）：再小就只剩「必然失败」，不如直接回超时。
pub const MIN_FETCH_TIMEOUT: Duration = Duration::from_millis(200);
/// 最多跟几跳重定向。
pub const MAX_REDIRECTS: usize = 5;
/// 请求体上限（256 KB）。`net.fetch` 是「取数据」用的，不是上传通道。
const MAX_REQUEST_BYTES: usize = 256 * 1024;
/// 允许的方法。只放行「取数据 / 提交表单」这一档，`TRACE` / `CONNECT` 不给。
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "HEAD", "PUT", "PATCH", "DELETE"];
/// 不允许插件自己设的请求头：这些头决定了「连谁、连多久、收多少」，
/// 由宿主与 HTTP 客户端决定，插件改一个就可能把闸门的假设顶掉
/// （典型是 `Host`）。
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "proxy-authorization",
];

/// 一次出站请求：已经过完闸门，只剩发字节。
#[derive(Debug, Clone)]
pub struct NetRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub max_bytes: u64,
    /// 本次实际可用预算：重定向后是「总预算减去已花掉的时间」。
    pub timeout: Duration,
}

/// 一次出站回包。只有真相干（状态码 / 位置 / 类型 / 体），不含任何判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetReply {
    pub status: u16,
    pub location: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub truncated: bool,
}

/// 出站传输实现方。
///
/// 必须是**同步**的：宿主调用本身是同步的、跑在插件 stdout 的帧泵里，
/// 实现方要自己做「异步转同步」（[`ReqwestTransport`] 起一个专用线程跑
/// current-thread 运行时），不能在这里 `block_on` 一个已经存在的运行时。
pub trait NetTransport: Send + Sync {
    fn send(&self, request: &NetRequest) -> Result<NetReply, HostApiError>;
}

/// 一次 `host/net.fetch` 的完整计划：闸门产物 + 请求形状。
///
/// 闸门通过之后才建得出来，所以 [`execute`] 不必再判「这个 url 该不该放行」，
/// 但**每一跳跳转**仍要重新过 [`guard_url`]——`domains` 留在这里就是为了那个。
#[derive(Debug, Clone)]
pub struct FetchPlan {
    pub url: Url,
    /// 该插件在清单里声明的域名清单（重定向逐跳复用）。
    pub domains: Vec<String>,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub max_bytes: u64,
    pub timeout: Duration,
}

/// 把 `params` 解成 [`FetchPlan`]：先判形状（`-32602`），再判资格（`6103`）。
///
/// 顺序是刻意的：参数畸形与「域名不在名单」是两件事，混在一起会让插件作者
/// 从错误码看不出该改参数还是该改清单。
pub fn plan_fetch(params: &Value, domains: &[String]) -> Result<FetchPlan, HostApiError> {
    let raw = match params.get("url") {
        Some(Value::String(raw)) if !raw.trim().is_empty() => raw.trim().to_string(),
        Some(Value::String(_)) => return Err(HostApiError::invalid_params("url 不能是空串")),
        None | Some(Value::Null) => return Err(HostApiError::invalid_params("缺少 url 参数")),
        Some(_) => return Err(HostApiError::invalid_params("url 必须是字符串")),
    };
    let url = Url::parse(&raw)
        .map_err(|error| HostApiError::invalid_params(format!("url 无法解析：{raw}（{error}）")))?;

    let method = match params.get("method") {
        None | Some(Value::Null) => "GET".to_string(),
        Some(Value::String(method)) => {
            let upper = method.trim().to_ascii_uppercase();
            if !ALLOWED_METHODS.contains(&upper.as_str()) {
                return Err(HostApiError::invalid_params(format!(
                    "method 只允许 {}，收到 {method}",
                    ALLOWED_METHODS.join(" / ")
                )));
            }
            upper
        }
        Some(_) => return Err(HostApiError::invalid_params("method 必须是字符串")),
    };

    let headers = parse_headers(params.get("headers"))?;
    let body = match params.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            if text.len() > MAX_REQUEST_BYTES {
                return Err(HostApiError::invalid_params(format!(
                    "body 超上限（{MAX_REQUEST_BYTES} 字节）"
                )));
            }
            Some(text.clone().into_bytes())
        }
        Some(_) => return Err(HostApiError::invalid_params("body 必须是字符串")),
    };
    if body.is_some() && matches!(method.as_str(), "GET" | "HEAD") {
        // GET/HEAD 带体在 HTTP 里没有定义良好的语义，各家客户端处理不一，
        // 索性不收：要提交数据就用 POST。
        return Err(HostApiError::invalid_params(format!(
            "{method} 不接受 body，请改用 POST"
        )));
    }

    let max_bytes = match number(params, "maxBytes") {
        Some(value) if value < 0.0 => {
            return Err(HostApiError::invalid_params("maxBytes 不能为负"))
        }
        Some(value) => (value as u64).clamp(1, MAX_FETCH_BYTES),
        None => DEFAULT_FETCH_BYTES,
    };
    let timeout = match number(params, "timeoutMs") {
        Some(value) if value <= 0.0 => {
            return Err(HostApiError::invalid_params("timeoutMs 必须为正数"))
        }
        Some(value) => Duration::from_millis((value as u64).clamp(
            MIN_FETCH_TIMEOUT.as_millis() as u64,
            MAX_FETCH_TIMEOUT.as_millis() as u64,
        )),
        None => DEFAULT_FETCH_TIMEOUT,
    };

    guard_url(&url, domains)?;
    Ok(FetchPlan {
        url,
        domains: domains.to_vec(),
        method,
        headers,
        body,
        max_bytes,
        timeout,
    })
}

fn number(params: &Value, key: &str) -> Option<f64> {
    match params.get(key) {
        Some(Value::Number(number)) => number.as_f64(),
        _ => None,
    }
}

fn parse_headers(raw: Option<&Value>) -> Result<Vec<(String, String)>, HostApiError> {
    let Some(value) = raw else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(object) = value.as_object() else {
        return Err(HostApiError::invalid_params("headers 必须是对象"));
    };
    let mut headers = Vec::with_capacity(object.len());
    for (name, value) in object {
        let Some(text) = value.as_str() else {
            return Err(HostApiError::invalid_params(format!(
                "headers.{name} 必须是字符串"
            )));
        };
        let lowered = name.trim().to_ascii_lowercase();
        if lowered.is_empty() || !lowered.is_ascii() {
            return Err(HostApiError::invalid_params(format!(
                "请求头名非法：{name}"
            )));
        }
        if FORBIDDEN_HEADERS.contains(&lowered.as_str()) {
            return Err(HostApiError::new(
                HostApiError::NET_DENIED,
                format!("请求头 {lowered} 由宿主决定，插件不能覆盖"),
            ));
        }
        if text.contains('\r') || text.contains('\n') {
            return Err(HostApiError::invalid_params(format!(
                "请求头 {name} 的值不得含换行"
            )));
        }
        headers.push((lowered, text.to_string()));
    }
    Ok(headers)
}

/// 一道完整的 url 闸门：协议 → 身份 → 域名清单 → 地址。**重定向逐跳都要过。**
pub fn guard_url(url: &Url, domains: &[String]) -> Result<(), HostApiError> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(HostApiError::new(
                HostApiError::NET_DENIED,
                format!("只允许 http / https，收到 {other}"),
            ))
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        // `https://user:pass@host` 会被不少日志原样打出来，也容易掩盖真正的
        // 目标主机（人眼读到的是前半段）。凭据请走请求头。
        return Err(HostApiError::new(
            HostApiError::NET_DENIED,
            "url 不得携带用户名或口令，凭据请走 headers".to_string(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| HostApiError::invalid_params("url 缺少主机名"))?;
    // `host_str` 对 IPv6 带方括号，去掉才能当 IP 解析。
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if !domain_allowed(bare, domains) {
        return Err(HostApiError::new(
            HostApiError::NET_DENIED,
            format!("域名 {bare} 不在清单声明的网络域名单内（allowedDomains）"),
        ));
    }
    let port = url.port_or_known_default().unwrap_or(80);
    guard_address(bare, port)
}

/// 域名清单匹配：精确或子域（清单写 `example.com` 就覆盖 `api.example.com`）。
///
/// `*.example.com` 等价于 `example.com` 的子域通配（不含裸域）——两种写法都
/// 允许，是因为清单作者对「要不要包含裸域」的理解常不一致，与其猜，不如让
/// 显式写法各自表达清楚。
pub fn domain_allowed(host: &str, domains: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    domains.iter().any(|entry| {
        let entry = entry.trim().trim_end_matches('.').to_ascii_lowercase();
        if entry.is_empty() {
            return false;
        }
        if let Some(suffix) = entry.strip_prefix("*.") {
            host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}"))
        } else {
            host == entry || host.ends_with(&format!(".{entry}"))
        }
    })
}

/// 地址闸门：域名先查「一眼就是本机」的名字，再解析；IP 直接判。
///
/// 解析出来的**每个**地址都要过（不止第一个）：一个域名同时给出公网与内网地址
/// 时，只查首条就可能放过内网的那条。
pub fn guard_address(host: &str, port: u16) -> Result<(), HostApiError> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return guard_ip(ip, host);
    }
    let lowered = host.trim_end_matches('.').to_ascii_lowercase();
    // `localhost` / `*.local`（mDNS）/ `*.internal`（云厂商内网域名）这些名字
    // 不解析也能判定，省一次 DNS，也避免 DNS 被本地劫持时判不出来。
    for name in [
        "localhost",
        "local",
        "internal",
        "home.arpa",
        "localhost.localdomain",
    ] {
        if lowered == name || lowered.ends_with(&format!(".{name}")) {
            return Err(HostApiError::new(
                HostApiError::NET_DENIED,
                format!("拒绝访问内网/回环主机 {host}"),
            ));
        }
    }
    let mut resolved = (host, port).to_socket_addrs().map_err(|error| {
        HostApiError::new(HostApiError::IO, format!("域名解析失败：{host}（{error}）"))
    })?;
    let mut seen = false;
    for addr in resolved.by_ref() {
        seen = true;
        guard_ip(addr.ip(), host)?;
    }
    if !seen {
        return Err(HostApiError::new(
            HostApiError::IO,
            format!("域名没有解析出任何地址：{host}"),
        ));
    }
    Ok(())
}

fn guard_ip(ip: IpAddr, host: &str) -> Result<(), HostApiError> {
    if let Some(reason) = internal_reason(ip) {
        return Err(HostApiError::new(
            HostApiError::NET_DENIED,
            format!("拒绝访问内网/回环地址：{host} 解析到 {ip}（{reason}）"),
        ));
    }
    Ok(())
}

/// 内网 / 回环 / 链路本地判定。命中就返回一个能直接写进错误消息的理由。
pub fn internal_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => internal_reason_v4(v4),
        IpAddr::V6(v6) => {
            // `::ffff:127.0.0.1` 这类映射地址要按 IPv4 判，否则整段 127/8 能绕过去。
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return internal_reason_v4(mapped);
            }
            internal_reason_v6(v6)
        }
    }
}

fn internal_reason_v4(ip: Ipv4Addr) -> Option<&'static str> {
    let octets = ip.octets();
    match octets {
        [0, ..] => Some("0.0.0.0/8"),
        [10, ..] => Some("10.0.0.0/8 内网"),
        [127, ..] => Some("127.0.0.0/8 回环"),
        [169, 254, ..] => Some("169.254.0.0/16 链路本地"),
        [172, second, ..] if (16..=31).contains(&second) => Some("172.16.0.0/12 内网"),
        [192, 168, ..] => Some("192.168.0.0/16 内网"),
        [192, 0, 0, ..] => Some("192.0.0.0/24 保留"),
        [100, second, ..] if (64..=127).contains(&second) => Some("100.64.0.0/10 运营商内网"),
        [198, second, ..] if second == 18 || second == 19 => Some("198.18.0.0/15 保留"),
        // 224+ 是组播与保留段（含 255.255.255.255 广播）。
        [first, ..] if first >= 224 => Some("组播/保留段"),
        _ => None,
    }
}

fn internal_reason_v6(ip: Ipv6Addr) -> Option<&'static str> {
    if ip.is_unspecified() {
        return Some(":: 未指定地址");
    }
    if ip.is_loopback() {
        return Some("::1 回环");
    }
    let segments = ip.segments();
    if segments[0] & 0xfe00 == 0xfc00 {
        return Some("fc00::/7 唯一本地地址");
    }
    if segments[0] & 0xffc0 == 0xfe80 {
        return Some("fe80::/10 链路本地");
    }
    if segments[0] & 0xff00 == 0xff00 {
        return Some("ff00::/8 组播");
    }
    None
}

/// 跑完一次抓取（含逐跳重定向）。
pub fn execute(transport: Arc<dyn NetTransport>, plan: &FetchPlan) -> Result<Value, HostApiError> {
    let started = std::time::Instant::now();
    let mut current = plan.url.clone();
    let mut method = plan.method.clone();
    let mut redirects = 0usize;

    loop {
        // 首跳在 `plan_fetch` 里过过一次；这里再过一次是无害的幂等动作，
        // 好处是「首跳」与「跳转后」走的是同一条判定路径，不会出现
        // 「改名之后忘了给首跳也校验」的漏。
        guard_url(&current, &plan.domains)?;

        let spent = started.elapsed();
        if spent >= plan.timeout {
            return Err(timeout_error(plan.timeout));
        }
        let budget = plan.timeout - spent;
        let request = NetRequest {
            url: current.to_string(),
            method: method.clone(),
            headers: plan.headers.clone(),
            body: if matches!(method.as_str(), "GET" | "HEAD") {
                None
            } else {
                plan.body.clone()
            },
            max_bytes: plan.max_bytes,
            timeout: budget,
        };
        let reply = run_with_budget(Arc::clone(&transport), request, budget)?;

        let location = reply.location.clone();
        let is_redirect = (300..400).contains(&reply.status);
        match (is_redirect, location) {
            (true, Some(location)) => {
                if redirects >= MAX_REDIRECTS {
                    return Err(HostApiError::new(
                        HostApiError::IO,
                        format!("重定向超过 {MAX_REDIRECTS} 跳"),
                    ));
                }
                let next = current.join(&location).map_err(|error| {
                    HostApiError::new(
                        HostApiError::NET_DENIED,
                        format!("重定向目标无法解析：{location}（{error}）"),
                    )
                })?;
                // 303 与 302/301 的常见实现一样按 GET 重发：重定向后的语义是
                // 「去拿那个资源」，不是「重放这次提交」。
                if matches!(reply.status, 301..=303) {
                    method = "GET".to_string();
                }
                redirects += 1;
                current = next;
            }
            (true, None) => {
                return Err(HostApiError::new(
                    HostApiError::IO,
                    format!("重定向响应缺少 Location（{}）", reply.status),
                ))
            }
            _ => {
                // 宿主这一层再兜一次截断：传输实现可能没按 `max_bytes` 收
                // （假实现 / 未来换引擎），结果面上的 `bytes` 必须与 `body`
                // 真实长度一致，否则调用方按 `bytes` 分配缓冲会读越界。
                let mut body = reply.body;
                let mut truncated = reply.truncated;
                if body.len() as u64 > plan.max_bytes {
                    body.truncate(plan.max_bytes as usize);
                    truncated = true;
                }
                return Ok(json!({
                    "url": current.as_str(),
                    "status": reply.status,
                    "contentType": reply.content_type,
                    "body": String::from_utf8_lossy(&body).to_string(),
                    "bytes": body.len(),
                    "truncated": truncated,
                    "redirects": redirects,
                }));
            }
        }
    }
}

fn timeout_error(budget: Duration) -> HostApiError {
    HostApiError::new(
        HostApiError::IO,
        format!("请求超时（{} ms）", budget.as_millis()),
    )
}

/// 在一段硬预算内跑一次传输，超预算就当场放弃。
///
/// 为什么不用 `recv_timeout` 之外的办法：`HostApi::call` 是同步的，而预算必须
/// 由**宿主**说了算——把超时寄托在传输实现上（比如 reqwest 的 `.timeout()`），
/// 换个实现就没有了。这里用一次性线程 + 有界等待，实现方卡死也只是丢一个线程。
fn run_with_budget(
    transport: Arc<dyn NetTransport>,
    request: NetRequest,
    budget: Duration,
) -> Result<NetReply, HostApiError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("net-fetch".to_string())
        .spawn(move || {
            let _ = tx.send(transport.send(&request));
        })
        .map_err(|error| {
            HostApiError::new(HostApiError::IO, format!("起网络请求线程失败：{error}"))
        })?;
    match rx.recv_timeout(budget) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(timeout_error(budget)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(HostApiError::new(
            HostApiError::IO,
            "网络请求线程异常退出".to_string(),
        )),
    }
}

/// 生产传输：reqwest + rustls（webpki 根证书）。
///
/// 客户端的重定向策略**必须**是 `none`：跳转由 [`execute`] 逐跳接管，
/// 否则「逐跳校验白名单」这条约束根本落不了地。
pub struct ReqwestTransport;

impl ReqwestTransport {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl NetTransport for ReqwestTransport {
    fn send(&self, request: &NetRequest) -> Result<NetReply, HostApiError> {
        // 调用点通常在 Tokio 的帧泵里：那里既不能 `block_on`（嵌套运行时会
        // panic），也不该被一个请求占住。所以在本线程起一个 current-thread
        // 运行时把它跑完——本线程由 `run_with_budget` 专为这次请求开出来。
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                HostApiError::new(HostApiError::IO, format!("建网络运行时失败：{error}"))
            })?;
        runtime.block_on(send_once(request))
    }
}

async fn send_once(request: &NetRequest) -> Result<NetReply, HostApiError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(request.timeout)
        .build()
        .map_err(|error| {
            HostApiError::new(HostApiError::IO, format!("建 HTTP 客户端失败：{error}"))
        })?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|error| {
        HostApiError::invalid_params(format!("method 无法编码：{}（{error}）", request.method))
    })?;
    let mut builder = client.request(method, &request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }
    let mut response = builder.send().await.map_err(|error| {
        if error.is_timeout() {
            HostApiError::new(HostApiError::IO, format!("请求超时：{}", request.url))
        } else {
            HostApiError::new(
                HostApiError::IO,
                format!("请求失败：{}（{error}）", request.url),
            )
        }
    })?;

    let status = response.status().as_u16();
    let location = header_text(response.headers(), reqwest::header::LOCATION);
    let content_type = header_text(response.headers(), reqwest::header::CONTENT_TYPE);

    // 多收 1 字节：用来分辨「刚好读满」和「还有更多」。若按 `max_bytes` 收，
    // 长度正好等于上限的响应会被误报成截断。
    let cap = request.max_bytes.saturating_add(1);
    let mut body = Vec::new();
    while (body.len() as u64) < cap {
        let chunk = response.chunk().await.map_err(|error| {
            HostApiError::new(
                HostApiError::IO,
                format!("读取响应体失败：{}（{error}）", request.url),
            )
        })?;
        let Some(chunk) = chunk else { break };
        let room = (cap - body.len() as u64) as usize;
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    let truncated = body.len() as u64 > request.max_bytes;
    if truncated {
        body.truncate(request.max_bytes as usize);
    }
    Ok(NetReply {
        status,
        location,
        content_type,
        body,
        truncated,
    })
}

fn header_text(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 落审计用的 url：保留 `scheme://host[:port]/path`，去掉 query 与 fragment。
///
/// 查询串里经常是签名、令牌、会话 id——审计库是长期留存的地方，把整串写进去
/// 等于让凭据再多活一遍。用户要看「连了谁」，`scheme+host+path` 已经够了。
pub fn audit_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_string();
    };
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

/// 一个从不真的联网的传输：只在单测里用，也方便别的 crate 拿它当桩。
///
/// 放在这里（而不是 `#[cfg(test)]`）是因为它没有外部依赖，且带 `!Send` 的
/// 返回队列用 `Mutex` 就够了。
pub struct OfflineTransport {
    replies: std::sync::Mutex<Vec<Result<NetReply, HostApiError>>>,
    requests: std::sync::Mutex<Vec<NetRequest>>,
    delay: Duration,
}

impl OfflineTransport {
    pub fn new(replies: Vec<Result<NetReply, HostApiError>>) -> Self {
        Self {
            replies: std::sync::Mutex::new(replies),
            requests: std::sync::Mutex::new(Vec::new()),
            delay: Duration::ZERO,
        }
    }

    /// 每次发送先睡这么久（用来验超时分支）。
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn sent(&self) -> Vec<NetRequest> {
        self.requests
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default()
    }
}

impl NetTransport for OfflineTransport {
    fn send(&self, request: &NetRequest) -> Result<NetReply, HostApiError> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        if let Ok(mut list) = self.requests.lock() {
            list.push(request.clone());
        }
        let next = self.replies.lock().ok().and_then(|mut list| {
            if list.is_empty() {
                None
            } else {
                Some(list.remove(0))
            }
        });
        next.unwrap_or_else(|| {
            Err(HostApiError::new(
                HostApiError::IO,
                "OfflineTransport 没有更多预置回包",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 单测里凡是要过 `guard_url` 的 url 都用公网 IP 字面量：`guard_address` 对
    // 域名要真解析一次，断网/断 DNS 的机器上会把「测闸门」变成「测网络」。

    fn reply(status: u16, location: Option<&str>, body: &[u8]) -> NetReply {
        NetReply {
            status,
            location: location.map(str::to_string),
            content_type: Some("text/plain".to_string()),
            body: body.to_vec(),
            truncated: false,
        }
    }

    fn domains(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    /// 协议与身份：非 http(s) 拒、带口令拒。
    #[test]
    fn url_gate_rejects_scheme_and_credentials() {
        let list = domains(&["example.com"]);
        let file = Url::parse("file:///C:/Windows/win.ini").expect("url");
        assert_eq!(
            guard_url(&file, &list).expect_err("file 必须拒").code,
            HostApiError::NET_DENIED
        );

        let creds = Url::parse("https://user:pass@example.com/x").expect("url");
        let error = guard_url(&creds, &list).expect_err("带口令必须拒");
        assert_eq!(error.code, HostApiError::NET_DENIED);
        assert!(error.message.contains("用户名或口令"));
    }

    /// 域名清单：精确、子域、通配三种写法都要对；裸域不在通配范围内。
    #[test]
    fn domain_whitelist_matches_exact_subdomain_and_wildcard() {
        assert!(domain_allowed("example.com", &domains(&["example.com"])));
        assert!(domain_allowed(
            "api.example.com",
            &domains(&["example.com"])
        ));
        assert!(!domain_allowed(
            "evilexample.com",
            &domains(&["example.com"])
        ));
        assert!(
            !domain_allowed("example.com", &domains(&["*.example.com"])),
            "通配不含裸域"
        );
        assert!(domain_allowed(
            "a.b.example.com",
            &domains(&["*.example.com"])
        ));
        assert!(!domain_allowed(
            "example.com.evil.test",
            &domains(&["example.com"])
        ));
        // 大小写与尾点都要归一，否则 `EXAMPLE.com.` 就绕过去了。
        assert!(domain_allowed(
            "API.EXAMPLE.COM",
            &domains(&["Example.Com."])
        ));
        assert!(
            !domain_allowed("anything.test", &domains(&[])),
            "空清单 = 全拒"
        );
    }

    /// 内网 / 回环 / 链路本地：域名写法与 IP 写法都要拦。
    #[test]
    fn address_gate_rejects_loopback_private_and_link_local() {
        for host in [
            "127.0.0.1",
            "127.1.2.3",
            "10.1.2.3",
            "172.20.0.9",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fd00::1",
            "fe80::1",
            "localhost",
            "printer.local",
            "metadata.google.internal",
        ] {
            let error = guard_address(host, 80).expect_err(&format!("{host} 必须被拒"));
            assert_eq!(error.code, HostApiError::NET_DENIED, "{host}");
        }
        // 公网地址直接放行（不联网，只判地址）。
        guard_address("93.184.216.34", 443).expect("公网 IP 应放行");
    }

    /// 超限必然标 truncated，且宿主的 `bytes` 与 `body` 长度一致。
    #[test]
    fn execute_marks_truncation_when_body_exceeds_limit() {
        let transport = Arc::new(OfflineTransport::new(vec![Ok(reply(
            200,
            None,
            &[b'x'; 500],
        ))]));
        let plan = FetchPlan {
            url: Url::parse("https://93.184.216.34/big").expect("url"),
            domains: domains(&["93.184.216.34"]),
            method: "GET".to_string(),
            headers: Vec::new(),
            body: None,
            max_bytes: 100,
            timeout: Duration::from_secs(5),
        };
        let result = execute(transport, &plan).expect("应成功");
        assert_eq!(result["truncated"], true);
        assert_eq!(result["bytes"], 100);
        assert_eq!(result["body"].as_str().unwrap_or("").len(), 100);
        assert_eq!(result["status"], 200);
        assert_eq!(result["redirects"], 0);
    }

    /// 重定向逐跳校验：跳到清单外的域名必须在**第二跳之前**被拦下。
    #[test]
    fn execute_rechecks_every_redirect_hop() {
        let transport = Arc::new(OfflineTransport::new(vec![
            Ok(reply(302, Some("https://evil.test/x"), b"")),
            Ok(reply(200, None, b"should not reach")),
        ]));
        let plan = FetchPlan {
            url: Url::parse("https://93.184.216.34/start").expect("url"),
            domains: domains(&["93.184.216.34"]),
            method: "GET".to_string(),
            headers: Vec::new(),
            body: None,
            max_bytes: 1024,
            timeout: Duration::from_secs(5),
        };
        let error = execute(Arc::clone(&transport) as Arc<dyn NetTransport>, &plan)
            .expect_err("跳转出去必须拒");
        assert_eq!(error.code, HostApiError::NET_DENIED);
        assert!(error.message.contains("不在清单"), "{}", error.message);
        let sent = transport.sent();
        assert_eq!(sent.len(), 1, "第二跳不该发出去");
    }

    /// 同域跳转要跟，并把 `redirects` 报出来。
    #[test]
    fn execute_follows_same_domain_redirect() {
        let transport = Arc::new(OfflineTransport::new(vec![
            Ok(reply(301, Some("/next"), b"")),
            Ok(reply(200, None, b"done")),
        ]));
        let plan = FetchPlan {
            url: Url::parse("https://93.184.216.34/start").expect("url"),
            domains: domains(&["93.184.216.34"]),
            method: "GET".to_string(),
            headers: Vec::new(),
            body: None,
            max_bytes: 1024,
            timeout: Duration::from_secs(5),
        };
        let result = execute(transport, &plan).expect("应成功");
        assert_eq!(result["redirects"], 1);
        assert_eq!(result["url"], "https://93.184.216.34/next");
        assert_eq!(result["body"], "done");
    }

    /// 超时：传输卡住时必须由宿主预算说话，而不是等它自己回来。
    #[test]
    fn execute_gives_up_when_budget_is_exhausted() {
        let transport = Arc::new(
            OfflineTransport::new(vec![Ok(reply(200, None, b"late"))])
                .with_delay(Duration::from_millis(600)),
        );
        let plan = FetchPlan {
            url: Url::parse("https://93.184.216.34/slow").expect("url"),
            domains: domains(&["93.184.216.34"]),
            method: "GET".to_string(),
            headers: Vec::new(),
            body: None,
            max_bytes: 1024,
            timeout: Duration::from_millis(200),
        };
        let started = std::time::Instant::now();
        let error = execute(transport, &plan).expect_err("必须超时");
        assert_eq!(error.code, HostApiError::IO);
        assert!(error.message.contains("超时"), "{}", error.message);
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "不该干等传输线程"
        );
    }

    /// `Host` 这类决定「连谁」的头不许插件覆盖。
    #[test]
    fn request_headers_cannot_override_host() {
        let error = plan_fetch(
            &json!({
                "url": "https://example.com/",
                "headers": { "Host": "internal.test" }
            }),
            &domains(&["example.com"]),
        )
        .expect_err("Host 头必须拒");
        assert_eq!(error.code, HostApiError::NET_DENIED);
    }
}
