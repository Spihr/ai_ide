//! 结构化日志（M5 任务 4）：JSONL 文件、按天滚动、可关的脱敏开关。
//!
//! ## 为什么不是「再开一个 tracing layer」了事
//!
//! 既有日志只写 stderr（`docs/02` §7 的绝对约定：stdout 只跑协议）。stderr 适合人看，
//! 但「日志里不含 API Key」这条验收要的是**能扫的文件**——于是这里多一条落盘的
//! JSONL 通道，与人读的那条并存：
//!
//! ```text
//! tracing 事件 ─┬─→ stderr（人类可读，保持原样）
//!               └─→ <data_dir>/logs/core-YYYY-MM-DD.jsonl（一行一条 JSON）
//! ```
//!
//! ## 三条硬约束
//!
//! 1. **一行一条 JSON**：写入前按 `\n` 切行，半行不落盘；进程被杀也不会留下
//!    拼接错乱的两条记录。
//! 2. **脱敏在出口做**：复用 `ai_ide_store::redact` 的同一套规则（键名 + `sk-` /
//!    `Bearer ` 等前缀），审计表与日志文件对「什么算密钥」只有一个答案。
//!    `AI_IDE_LOG_REDACT=off` 可关（仅用于本地排查，脱敏默认开）。
//! 3. **按天滚动 + 单文件上限**：文件名带日期，超过 [`MAX_BYTES`] 再加序号后缀，
//!    避免一个长跑进程把单文件写成几个 G。
//!
//! ## 开关
//!
//! | 环境变量 | 默认 | 作用 |
//! |---|---|---|
//! | `AI_IDE_LOG_DIR` | `<data_dir>/logs` | 日志目录 |
//! | `AI_IDE_LOG_JSONL` | `on` | 设为 `off` / `0` / `false` / `no` 则完全不落盘 |
//! | `AI_IDE_LOG_REDACT` | `on` | 设为上述值则关闭脱敏（仅本地排查用） |
//! | `AI_IDE_LOG_PROBE` | 关 | 启动时打一条含假密钥的日志，供 CI 的密钥扫描门禁取样 |

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ai_ide_store::redact_text;

/// 日志目录覆盖项。
pub const LOG_DIR_ENV: &str = "AI_IDE_LOG_DIR";
/// JSONL 落盘开关（`off` 关闭）。
pub const LOG_JSONL_ENV: &str = "AI_IDE_LOG_JSONL";
/// 脱敏开关（`off` 关闭）。
pub const LOG_REDACT_ENV: &str = "AI_IDE_LOG_REDACT";
/// 启动探针（CI 取用）。
pub const LOG_PROBE_ENV: &str = "AI_IDE_LOG_PROBE";

/// 单个 JSONL 文件的滚动上限（32 MiB）。超出后追加序号后缀。
pub const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// 文件名前缀；同日多份时形如 `core-2026-10-22-1.jsonl`。
const FILE_PREFIX: &str = "core";

/// 读一个「开关型」环境变量：只有明确的假值才算关。
fn flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => {
            let lowered = value.trim().to_ascii_lowercase();
            !matches!(lowered.as_str(), "" | "0" | "off" | "false" | "no")
        }
        Err(_) => default,
    }
}

/// JSONL 是否落盘。
pub fn jsonl_enabled() -> bool {
    flag(LOG_JSONL_ENV, true)
}

/// 是否对日志行脱敏（默认开）。
pub fn redact_enabled() -> bool {
    flag(LOG_REDACT_ENV, true)
}

/// 日志目录：`AI_IDE_LOG_DIR` 优先，否则与 `providers.json` / `sessions.db` 同级。
pub fn log_dir() -> PathBuf {
    match std::env::var_os(LOG_DIR_ENV).filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => crate::llm_runtime::default_data_dir().join("logs"),
    }
}

/// Unix 天数 → `(年, 月, 日)`（Howard Hinnant 的 `civil_from_days`）。
///
/// 自己算而不是拉一个时间库：`crates/store` 的既有口径是「不引入 chrono」，
/// 这里也保持一致——只为了文件名上一个日期，不值一个新依赖。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// 某个时刻的本地日期串（`YYYY-MM-DD`）。
///
/// 用 `SystemTime` 的秒数直接换算 UTC 日期：日志文件按 UTC 日切分，
/// 与「跨时区团队读同一份日志」的预期一致，也与 `tracing` 默认的时间戳同口径。
fn date_of(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

struct State {
    /// 尚未写完的残行（等下一个 `\n`）。
    pending: Vec<u8>,
    file: Option<File>,
    /// 当前文件对应的日期。
    day: String,
    /// 同日内的序号（0 为 `core-<day>.jsonl`）。
    index: u32,
    /// 当前文件已写字节数。
    written: u64,
}

struct Inner {
    dir: PathBuf,
    redact: bool,
    max_bytes: u64,
    state: Mutex<State>,
}

/// 按天滚动的 JSONL 写入器：`tracing_subscriber` 的 `MakeWriter` 实现。
///
/// 克隆是廉价的（内部 `Arc`），多个 layer / 多个线程写同一份文件不会交错。
#[derive(Clone)]
pub struct DailyJsonlWriter {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DailyJsonlWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DailyJsonlWriter")
            .field("dir", &self.inner.dir)
            .field("redact", &self.inner.redact)
            .field("max_bytes", &self.inner.max_bytes)
            .finish()
    }
}

impl DailyJsonlWriter {
    /// 用环境变量的开关构造（脱敏默认开、上限 [`MAX_BYTES`]）。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_options(dir, redact_enabled(), MAX_BYTES)
    }

    /// 显式指定目录、脱敏开关与单文件上限。
    pub fn with_options(dir: impl Into<PathBuf>, redact: bool, max_bytes: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir: dir.into(),
                redact,
                max_bytes: max_bytes.max(1),
                state: Mutex::new(State {
                    pending: Vec::new(),
                    file: None,
                    day: String::new(),
                    index: 0,
                    written: 0,
                }),
            }),
        }
    }

    /// 日志目录。
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// 是否脱敏。
    pub fn redact(&self) -> bool {
        self.inner.redact
    }

    /// 把缓冲里的残行也写出去（进程退出前调用；没有 `\n` 也算一条）。
    pub fn flush_pending(&self) {
        let mut state = match self.inner.state.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if state.pending.is_empty() {
            return;
        }
        let line = std::mem::take(&mut state.pending);
        self.emit(&mut state, &line);
        if let Some(file) = state.file.as_mut() {
            let _ = file.flush();
        }
    }

    /// 写一行（不带 `\n`）：脱敏 → 落盘 → 更新计数。
    ///
    /// 落盘失败**不 panic**：日志是旁路，写不进去也不该把内核带崩。
    fn emit(&self, state: &mut State, line: &[u8]) {
        let raw = String::from_utf8_lossy(line);
        let text = if self.inner.redact {
            redact_text(&raw)
        } else {
            raw.into_owned()
        };
        let mut bytes = text.into_bytes();
        bytes.push(b'\n');

        if let Err(err) = self.ensure_file(state, bytes.len() as u64) {
            // 目录不可写 / 磁盘满：只在 stderr 上说一次由调用方决定，这里静默。
            let _ = err;
            return;
        }
        if let Some(file) = state.file.as_mut() {
            if file.write_all(&bytes).is_ok() {
                state.written += bytes.len() as u64;
            }
        }
    }

    /// 按需打开（或滚动）当天文件。
    fn ensure_file(&self, state: &mut State, add: u64) -> io::Result<()> {
        let day = date_of(SystemTime::now());
        let roll = state.file.is_none()
            || state.day != day
            || state.written.saturating_add(add) > self.inner.max_bytes;
        if !roll {
            return Ok(());
        }

        if state.day != day {
            state.day = day.clone();
            state.index = 0;
            state.written = 0;
        } else if state.file.is_some() {
            // 同一天写满了：换序号继续。
            state.index = state.index.saturating_add(1);
            state.written = 0;
        }

        std::fs::create_dir_all(&self.inner.dir)?;
        let path = self.path_for(&state.day, state.index);
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        let file = options.open(&path)?;
        state.written = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        state.file = Some(file);
        Ok(())
    }

    /// 文件名：`core-<day>.jsonl` / `core-<day>-<n>.jsonl`。
    fn path_for(&self, day: &str, index: u32) -> PathBuf {
        if index == 0 {
            self.inner.dir.join(format!("{FILE_PREFIX}-{day}.jsonl"))
        } else {
            self.inner
                .dir
                .join(format!("{FILE_PREFIX}-{day}-{index}.jsonl"))
        }
    }
}

impl Write for DailyJsonlWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = match self.inner.state.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        state.pending.extend_from_slice(buf);

        // 按 `\n` 切出完整行；最后一段（没有换行）留在 pending 里等下一批。
        while let Some(pos) = state.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = state.pending.drain(..=pos).collect();
            let payload = &line[..line.len() - 1];
            if payload.iter().all(|byte| byte.is_ascii_whitespace()) {
                continue;
            }
            self.emit(&mut state, payload);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = match self.inner.state.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if let Some(file) = state.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DailyJsonlWriter {
    type Writer = DailyJsonlWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 一行一条 JSON 的 `tracing` 事件格式化器。
///
/// 刻意不用 `tracing-subscriber` 的 `json` feature：那会为内核多引一个
/// `tracing-serde` 依赖，而这里只需要 `timestamp` / `level` / `target` / 事件字段
/// 四样东西，`serde_json` 本就在依赖里。自己拼还能把 schema 钉死——
/// 下游扫描器与 `grep` 依赖的就是这个固定形状。
pub struct JsonLinesFormatter;

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for JsonLinesFormatter
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let metadata = event.metadata();
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);

        let mut object = serde_json::Map::new();
        object.insert(
            "timestamp".to_string(),
            serde_json::Value::String(iso8601_now()),
        );
        object.insert(
            "level".to_string(),
            serde_json::Value::String(metadata.level().to_string()),
        );
        object.insert(
            "target".to_string(),
            serde_json::Value::String(metadata.target().to_string()),
        );
        if let Some(scope) = ctx.event_scope() {
            if let Some(span) = scope.from_root().last() {
                object.insert(
                    "span".to_string(),
                    serde_json::Value::String(span.name().to_string()),
                );
            }
        }
        for (key, value) in visitor.fields {
            object.insert(key, value);
        }

        let line = serde_json::to_string(&serde_json::Value::Object(object))
            .map_err(|_| std::fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

/// 把事件字段收成 `(键, JSON 值)`；非字符串一律转字符串。
#[derive(Default)]
struct JsonVisitor {
    fields: Vec<(String, serde_json::Value)>,
}

impl JsonVisitor {
    fn push(&mut self, field: &tracing::field::Field, value: serde_json::Value) {
        self.fields.push((field.name().to_string(), value));
    }
}

impl tracing::field::Visit for JsonVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field, serde_json::Value::String(value.to_string()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.push(field, serde_json::Value::Bool(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.push(field, serde_json::Value::from(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.push(field, serde_json::Value::from(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.push(field, serde_json::Value::from(value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.push(field, serde_json::Value::String(format!("{value:?}")));
    }
}

/// 当前时刻的 ISO-8601（UTC，毫秒精度）。
fn iso8601_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// 启动探针：`AI_IDE_LOG_PROBE=1` 时打一条含**假**密钥的日志，供 CI 扫描门禁取样。
///
/// 假密钥刻意长得像真的（`sk-` 前缀 + 32 位正文），保证「扫描器能命中明文」这件事
/// 本身也被验证——否则门禁可能只是扫了个空目录。
pub fn probe_if_requested() {
    if !flag(LOG_PROBE_ENV, false) {
        return;
    }
    tracing::info!(
        api_key = "sk-probe0000000000000000000000000000000000000000",
        authorization = "Bearer probe00000000000000000000000000000000",
        "日志脱敏探针：以上两个字段应为打码值"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|dur| dur.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("ai-ide-log-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn read_all(dir: &Path) -> String {
        let mut out = String::new();
        for entry in std::fs::read_dir(dir).expect("读目录") {
            let path = entry.expect("目录项").path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                out.push_str(&std::fs::read_to_string(&path).expect("读日志"));
            }
        }
        out
    }

    #[test]
    fn civil_date_matches_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-10-22 = 2026-01-01(20454) + 294 天
        assert_eq!(civil_from_days(20_748), (2026, 10, 22));
    }

    #[test]
    fn file_name_carries_the_utc_day() {
        let day = date_of(SystemTime::now());
        assert_eq!(day.len(), 10, "日期串应为 YYYY-MM-DD：{day}");
        assert_eq!(&day[4..5], "-");
        assert_eq!(&day[7..8], "-");
    }

    #[test]
    fn writes_one_json_object_per_line() {
        let dir = temp_dir("lines");
        let mut writer = DailyJsonlWriter::with_options(&dir, true, MAX_BYTES);
        writer.write_all(b"{\"a\":1}\n{\"b\":2}\n").expect("写入");
        writer.flush().expect("刷新");

        let text = read_all(&dir);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "应有两条：{text}");
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).expect("每行都是 JSON");
            assert!(value.is_object());
        }
    }

    #[test]
    fn partial_line_waits_for_newline() {
        let dir = temp_dir("partial");
        let mut writer = DailyJsonlWriter::with_options(&dir, true, MAX_BYTES);
        writer.write_all(b"{\"half\":").expect("写入");
        writer.flush().expect("刷新");
        assert!(read_all(&dir).is_empty(), "没有换行就不该落盘");

        writer.write_all(b"1}\n").expect("补完");
        writer.flush().expect("刷新");
        assert_eq!(read_all(&dir).trim(), "{\"half\":1}");
    }

    #[test]
    fn secrets_are_masked_by_default() {
        let dir = temp_dir("mask");
        let mut writer = DailyJsonlWriter::with_options(&dir, true, MAX_BYTES);
        let secret = "sk-live0123456789abcdefghijklmn";
        writer
            .write_all(format!("{{\"note\":\"key={secret}\"}}\n").as_bytes())
            .expect("写入");
        writer.flush().expect("刷新");

        let text = read_all(&dir);
        assert!(!text.contains(secret), "明文密钥不该落盘：{text}");
        assert!(text.contains("***"), "应留下打码痕迹：{text}");
    }

    #[test]
    fn redaction_can_be_switched_off() {
        let dir = temp_dir("no-mask");
        let mut writer = DailyJsonlWriter::with_options(&dir, false, MAX_BYTES);
        writer
            .write_all(b"{\"note\":\"sk-live0123456789abcdefghijklmn\"}\n")
            .expect("写入");
        writer.flush().expect("刷新");
        assert!(read_all(&dir).contains("sk-live0123456789abcdefghijklmn"));
    }

    #[test]
    fn rolls_when_file_limit_is_reached() {
        let dir = temp_dir("roll");
        // 上限压到 60 字节：第二条必须换文件。
        let mut writer = DailyJsonlWriter::with_options(&dir, true, 60);
        writer
            .write_all(b"{\"line\":\"0123456789012345678901234567890123456789\"}\n")
            .expect("第一条");
        writer
            .write_all(b"{\"line\":\"abcdefghij\"}\n")
            .expect("第二条");
        writer.flush().expect("刷新");

        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("读目录")
            .filter_map(|entry| entry.ok().map(|item| item.path()))
            .collect();
        assert_eq!(files.len(), 2, "应滚出第二份文件：{files:?}");
        assert!(files
            .iter()
            .any(|path| path.to_string_lossy().contains("-1.jsonl")));
    }

    #[test]
    fn tracing_layer_writes_jsonl() {
        use tracing_subscriber::layer::SubscriberExt;

        let dir = temp_dir("tracing");
        let writer = DailyJsonlWriter::with_options(&dir, true, MAX_BYTES);
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .event_format(JsonLinesFormatter)
                .with_writer(writer),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(api_key = "sk-abcdef0123456789abcdef", "握手完成");
        });

        let text = read_all(&dir);
        assert!(text.contains("握手完成"), "消息应落盘：{text}");
        assert!(
            !text.contains("sk-abcdef0123456789abcdef"),
            "密钥应打码：{text}"
        );
        let line = text.lines().next().expect("一行");
        let value: serde_json::Value = serde_json::from_str(line).expect("合法 JSON");
        assert_eq!(value["level"], "INFO");
    }

    #[test]
    fn probe_only_fires_on_demand() {
        // 未设 `AI_IDE_LOG_PROBE` 时不该有任何副作用；这里只验证开关解析本身。
        assert!(!flag("AI_IDE_LOG_PROBE_UNSET_FOR_TEST", false));
    }
}
