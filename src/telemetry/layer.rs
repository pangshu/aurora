//! 链路 ID（trace-id）与请求级作用域。
//!
//! 遵循 W3C Trace Context：`trace_id` 为 **32 位十六进制**（16 字节，全 0 非法）。
//!
//! 请求进入时由 `http::middleware::trace` 解析（优先继承上游 `traceparent`）
//! 或生成新 id，并以 [`scope`] 建立 task-local 作用域；作用域内的
//! [`current_trace_id`] 返回该请求的 id，从而让日志、统一响应体与
//! `X-Trace-Id` 响应头共享同一个链路标识。

use std::fmt;
use std::str::FromStr;

use axum::http::HeaderMap;
use uuid::Uuid;

tokio::task_local! {
    static CURRENT_TRACE_ID: TraceId;
}

/// W3C trace-id（32 位十六进制字符串）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraceId(String);

impl TraceId {
    /// 生成一个新的 trace-id。
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4().simple().to_string())
    }

    /// 校验并构造：仅接受 32 位十六进制且不全为 `0` 的字符串。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.len() != 32 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        if value.chars().all(|c| c == '0') {
            return None;
        }
        Some(Self(value.to_ascii_lowercase()))
    }

    /// 以字符串切片形式返回。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TraceId {
    type Err = InvalidTraceId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(InvalidTraceId)
    }
}

/// [`TraceId`] 解析失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid trace-id: expected 32 hex characters (not all zero)")]
pub struct InvalidTraceId;

/// 生成一个新的 32 位十六进制 trace-id。
#[must_use]
pub fn new_trace_id() -> String {
    TraceId::new().as_str().to_string()
}

/// 在给定 trace-id 的作用域内执行 `future`。
pub async fn scope<F>(trace_id: TraceId, future: F) -> F::Output
where
    F: std::future::Future,
{
    CURRENT_TRACE_ID.scope(trace_id, future).await
}

/// 读取当前请求作用域内的 trace-id。
#[must_use]
pub fn try_current_trace_id() -> Option<TraceId> {
    CURRENT_TRACE_ID.try_with(TraceId::clone).ok()
}

/// 作用域外读取 trace-id 时的占位值。
///
/// 刻意**不**生成随机 id：响应体 / 日志里出现一个「格式正确但谁也查不到」的
/// trace-id，比明确写 `unknown` 更容易误导排查。
pub const UNKNOWN_TRACE_ID: &str = "unknown";

/// 回写 trace-id 的响应头名。
///
/// 同时是「已经离开请求作用域」的调用方取回 trace-id 的唯一途径：最外层中间件在
/// `next.run` 返回之后才 spawn 后台任务，那时 task-local 已回收，只能读这个头。
pub const TRACE_ID_HEADER: &str = "x-trace-id";

/// 读取当前请求的 trace-id。
///
/// 若不在请求作用域内（启动期、单元测试、`tokio::spawn` / `spawn_blocking` 出来的
/// 后台任务——task-local **不跨任务继承**），返回 [`UNKNOWN_TRACE_ID`]。需要区分
/// 「没有」与「有」时用 [`try_current_trace_id`]。
#[must_use]
pub fn current_trace_id() -> String {
    try_current_trace_id().map_or_else(
        || UNKNOWN_TRACE_ID.to_string(),
        |trace_id| trace_id.as_str().to_string(),
    )
}

/// 从请求头解析 trace-id：优先 W3C `traceparent`，其次 `X-Trace-Id`。
#[must_use]
pub fn trace_id_from_headers(headers: &HeaderMap) -> Option<TraceId> {
    if let Some(value) = headers.get("traceparent").and_then(|v| v.to_str().ok())
        && let Some(trace_id) = parse_traceparent(value)
    {
        return Some(trace_id);
    }
    headers
        .get("x-trace-id")
        .and_then(|v| v.to_str().ok())
        .and_then(TraceId::parse)
}

/// 解析 W3C `traceparent`：`{version}-{trace-id}-{parent-id}-{flags}`。
///
/// 按规范校验全部四段（`docs/` 之外的现实原因：这一段来自上游代理，格式错的
/// 值会被一路带进日志与响应头）：
/// - `version`：2 位十六进制，**`ff` 是保留值，非法**；
/// - `trace-id`：32 位十六进制且不全 0；
/// - `parent-id`：16 位十六进制且不全 0；
/// - `flags`：2 位十六进制。
///
/// 只接受 4 段：未来版本的扩展字段我们无法解释，直接放弃继承、新生成一个。
fn parse_traceparent(value: &str) -> Option<TraceId> {
    let mut parts = value.trim().split('-');
    let version = parts.next()?;
    if version.len() != 2
        || !version.chars().all(|c| c.is_ascii_hexdigit())
        || version.eq_ignore_ascii_case("ff")
    {
        return None;
    }

    let trace_id = parts.next()?;
    let parent_id = parts.next()?;
    let flags = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    if !is_hex_of(parent_id, 16) || parent_id.chars().all(|c| c == '0') {
        return None;
    }
    if !is_hex_of(flags, 2) {
        return None;
    }

    TraceId::parse(trace_id)
}

/// `value` 是否恰好 `len` 位 ASCII 十六进制。
fn is_hex_of(value: &str, len: usize) -> bool {
    value.len() == len && value.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_generate_32_hex_trace_id() {
        let id = new_trace_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn should_reject_invalid_trace_id() {
        assert!(TraceId::parse("").is_none());
        assert!(TraceId::parse("abc").is_none());
        assert!(TraceId::parse(&"0".repeat(32)).is_none());
        assert!(TraceId::parse(&"z".repeat(32)).is_none());
        assert!(TraceId::parse(&"a".repeat(32)).is_some());
    }

    #[test]
    fn should_prefer_traceparent_over_x_trace_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
                .parse()
                .unwrap(),
        );
        headers.insert("x-trace-id", "a".repeat(32).parse().unwrap());
        assert_eq!(
            trace_id_from_headers(&headers).map(|t| t.as_str().to_string()),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }

    #[test]
    fn should_fall_back_to_x_trace_id_when_traceparent_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", "garbage".parse().unwrap());
        headers.insert("x-trace-id", "b".repeat(32).parse().unwrap());
        assert_eq!(
            trace_id_from_headers(&headers).map(|t| t.as_str().to_string()),
            Some("b".repeat(32))
        );
    }

    #[tokio::test]
    async fn should_read_scoped_trace_id() {
        let id = TraceId::parse(&"c".repeat(32)).unwrap();
        let observed = scope(id.clone(), async { current_trace_id() }).await;
        assert_eq!(observed, id.as_str());
        // 作用域外明确给出占位值，而不是造一个查不到的随机 id
        assert_eq!(current_trace_id(), UNKNOWN_TRACE_ID);
        assert!(try_current_trace_id().is_none());
    }

    #[test]
    fn should_validate_all_traceparent_fields() {
        let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let parent = "00f067aa0ba902b7";
        let ok = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("traceparent", value.parse().unwrap());
            trace_id_from_headers(&headers).map(|t| t.as_str().to_string())
        };

        assert_eq!(
            ok(&format!("00-{trace_id}-{parent}-01")),
            Some(trace_id.to_string())
        );
        // version = ff 是 W3C 保留值，非法
        assert_eq!(ok(&format!("ff-{trace_id}-{parent}-01")), None);
        // 缺 parent-id / flags
        assert_eq!(ok(&format!("00-{trace_id}")), None);
        assert_eq!(ok(&format!("00-{trace_id}-{parent}")), None);
        // parent-id 全 0
        assert_eq!(ok(&format!("00-{trace_id}-{}-01", "0".repeat(16))), None);
        // flags 不是 2 位
        assert_eq!(ok(&format!("00-{trace_id}-{parent}-1")), None);
        // 多出一段（未来版本扩展字段）：不猜测，直接不继承
        assert_eq!(ok(&format!("00-{trace_id}-{parent}-01-extra")), None);
    }
}
