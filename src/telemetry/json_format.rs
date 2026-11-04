//! JSON 日志格式化：把 `trace_id` 提升到事件**顶层**。
//!
//! `tracing-subscriber` 自带的 JSON formatter 把事件字段放在 `fields` 子对象里、
//! span 字段放在 `span` / `spans` 里，`trace_id` 只出现在后者。日志采集端按顶层
//! 字段建索引时，`trace_id` 因此不可见——「请求有 id、日志查不到」的根因就在这里。
//!
//! 这里换成自己实现的格式化器，schema 固定为：
//!
//! ```json
//! {"timestamp":"…","level":"INFO","target":"…","trace_id":"…","message":"…","spans":[…]}
//! ```
//!
//! 事件自身字段**平铺到顶层**（不再有 `fields`），`trace_id` 始终存在：
//! 取值来自 task-local（请求作用域，权威）→ 事件自带字段 → [`UNKNOWN_TRACE_ID`]。
//!
//! 关闭 span 列表会丢掉 `method` / `path`，所以默认保留；span 字段由
//! `FormattedFields<JsonFields>` 反序列化而来（与 stock 实现同一条路径）。

use std::fmt;

use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

use crate::telemetry::{UNKNOWN_TRACE_ID, try_current_trace_id};

/// 顶层保留键：事件里的同名字段会被丢弃，避免覆盖元数据。
const RESERVED: [&str; 5] = ["timestamp", "level", "target", "trace_id", "spans"];

/// 带 `trace_id` 顶层字段的 JSON 事件格式化器。
///
/// `Copy` + 无状态，控制台与文件两条 `fmt` 层共用同一份实例。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct JsonWithTraceId {
    /// 是否输出 `spans` 数组（root → leaf）。
    pub(crate) spans: bool,
}

impl JsonWithTraceId {
    /// 保留 span 上下文（请求的 `method` / `path` 来自这里）。
    pub(crate) fn with_spans() -> Self {
        Self { spans: true }
    }
}

impl<S, N> FormatEvent<S, N> for JsonWithTraceId
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();

        let mut fields = Map::new();
        event.record(&mut FieldVisitor(&mut fields));

        let mut root = Map::new();
        root.insert(
            "timestamp".to_owned(),
            json!(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
        );
        root.insert("level".to_owned(), json!(metadata.level().as_str()));
        root.insert("target".to_owned(), json!(metadata.target()));
        root.insert("trace_id".to_owned(), json!(resolve_trace_id(&fields)));

        for (key, value) in fields {
            if !RESERVED.contains(&key.as_str()) {
                root.insert(key, value);
            }
        }

        if self.spans
            && let Some(spans) = collect_spans(ctx)
        {
            root.insert("spans".to_owned(), spans);
        }

        writeln!(writer, "{}", Value::Object(root))
    }
}

/// 决定本次事件的 `trace_id`。
///
/// task-local 优先：`access_log` 之类自己写了 `trace_id` 字段的调用点，
/// 在请求作用域内外都能得到一致的值（作用域外两者都落到 `unknown`）。
fn resolve_trace_id(fields: &Map<String, Value>) -> String {
    if let Some(trace_id) = try_current_trace_id() {
        return trace_id.as_str().to_owned();
    }
    fields
        .get("trace_id")
        .and_then(Value::as_str)
        .map_or_else(|| UNKNOWN_TRACE_ID.to_owned(), str::to_owned)
}

/// 把当前 span 链（root → leaf）收集成数组。
///
/// span 字段的 JSON 由 `fmt` 层的 `JsonFields` 写入 `FormattedFields`，
/// 这里解析回对象；解析失败时保留空的字段集（不让一条坏 span 吃掉整行日志）。
fn collect_spans<S, N>(ctx: &FmtContext<'_, S, N>) -> Option<Value>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    let leaf = ctx.lookup_current()?;
    let mut spans = Vec::new();
    for span in leaf.scope().from_root() {
        let mut object = span
            .extensions()
            .get::<FormattedFields<N>>()
            .and_then(|fields| serde_json::from_str::<Value>(fields.as_str()).ok())
            .and_then(|value| match value {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();
        object.insert(
            "name".to_owned(),
            Value::String(span.metadata().name().to_owned()),
        );
        spans.push(Value::Object(object));
    }
    Some(Value::Array(spans))
}

/// 按写入类型落成 JSON 标量。
///
/// `%x`（Display）与 `?x`（Debug）都会走到 `record_debug`：tracing 用两个包装类型
/// 分别代理 Display / Debug，`format!("{value:?}")` 因此对两者都给出正确文本
/// （`message` 字段的 `fmt::Arguments` 也一样）。
struct FieldVisitor<'a>(&'a mut Map<String, Value>);

impl FieldVisitor<'_> {
    fn insert(&mut self, field: &Field, value: Value) {
        self.0.insert(field.name().to_owned(), value);
    }
}

impl Visit for FieldVisitor<'_> {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, json!(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, json!(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, json!(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // JSON 没有 NaN / Infinity 字面量，回落成字符串（否则整行序列化失败）
        let value = serde_json::Number::from_f64(value)
            .map_or_else(|| json!(value.to_string()), Value::Number);
        self.insert(field, value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, json!(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, json!(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert(field, json!(format!("{value:?}")));
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;
    use crate::telemetry::{TraceId, scope};

    /// 把格式化结果收集到内存。
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn line(&self) -> String {
            let bytes = self.0.lock().expect("捕获锁").clone();
            String::from_utf8(bytes).expect("日志必须是 UTF-8")
        }
    }

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("捕获锁").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn subscriber(capture: Capture) -> impl Subscriber + 'static {
        tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .event_format(JsonWithTraceId::with_spans())
                .with_writer(capture),
        )
    }

    fn parse(capture: &Capture) -> Value {
        let line = capture.line();
        serde_json::from_str(line.trim()).expect("每条事件必须是合法的单行 JSON")
    }

    #[test]
    fn should_flatten_fields_and_default_to_unknown_trace_id() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(subscriber(capture.clone()));
        tracing::info!(attempt = 3u64, "started");

        let json = parse(&capture);
        assert_eq!(json["level"].as_str(), Some("INFO"));
        assert_eq!(json["message"].as_str(), Some("started"));
        assert_eq!(json["attempt"].as_u64(), Some(3));
        assert_eq!(
            json["trace_id"].as_str(),
            Some(UNKNOWN_TRACE_ID),
            "作用域外必须显式给出 unknown，而不是漏字段"
        );
        assert!(
            json.get("fields").is_none(),
            "字段必须平铺到顶层，不能留在 fields 子对象里"
        );
    }

    #[tokio::test]
    async fn should_prefer_scoped_trace_id_over_event_field() {
        let capture = Capture::default();
        let trace_id = TraceId::parse(&"a".repeat(32)).expect("合法 trace-id");
        let expected = trace_id.as_str().to_owned();

        let _guard = tracing::subscriber::set_default(subscriber(capture.clone()));
        scope(trace_id, async {
            // 事件自带的 trace_id 必须被 task-local 覆盖（两处不一致时以作用域为准）
            tracing::info!(trace_id = "from-event", "inside");
        })
        .await;

        let json = parse(&capture);
        assert_eq!(json["trace_id"].as_str(), Some(expected.as_str()));
    }

    #[test]
    fn should_include_span_fields_in_order() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(subscriber(capture.clone()));

        let outer = tracing::info_span!("request", method = "GET", path = "/users");
        let entered = outer.enter();
        tracing::info!("inner");
        drop(entered);

        let json = parse(&capture);
        let spans = json["spans"].as_array().expect("spans 必须是数组");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["name"].as_str(), Some("request"));
        assert_eq!(spans[0]["method"].as_str(), Some("GET"));
        assert_eq!(spans[0]["path"].as_str(), Some("/users"));
    }

    #[test]
    fn should_not_let_event_fields_override_metadata() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(subscriber(capture.clone()));
        tracing::info!(timestamp = "forged", "x");

        let json = parse(&capture);
        assert_ne!(
            json["timestamp"].as_str(),
            Some("forged"),
            "保留键不能被事件字段覆盖"
        );
        assert!(json["timestamp"].as_str().is_some());
    }
}
