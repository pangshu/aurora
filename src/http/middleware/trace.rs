//! 链路追踪中间件。
//!
//! 为每个请求解析 / 生成 trace-id，建立带 `trace_id` 字段的 span，并回写
//! `X-Trace-Id` 响应头（32 位十六进制 W3C trace-id）。
//!
//! **继承上游 id 需满足信任条件**：只有直连对端命中
//! `[server].trusted_proxies`（且 `trust_proxy = true`）时才采纳
//! `traceparent` / `X-Trace-Id`；否则一律生成新 id。否则任意客户端都能
//! 指定 trace-id，把攻击流量混进正常链路、污染溯源。
//!
//! 不用 `tower-http::trace::TraceLayer`（`docs/02` §6）：
//! 这里改用自定义 `from_fn` 层，因为 trace-id 的「继承上游 `traceparent` / 生成 /
//! 写入 task-local 作用域 / 回写 `X-Trace-Id`」需在同一层内完成（见 `docs/02` §8、
//! `docs/05` §6），`TraceLayer` 无法覆盖该组合。待 docs/02 §6 同步实现口径后移除本 TODO。

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

use crate::telemetry::{TRACE_ID_HEADER, TraceId, scope, trace_id_from_headers};
use crate::utils::ProxyTrust;

/// 响应头名：`X-Trace-Id`。
fn trace_header() -> HeaderName {
    HeaderName::from_static(TRACE_ID_HEADER)
}

/// 解析或生成 trace-id，建立 span 与作用域，并回写 `X-Trace-Id`。
pub(crate) async fn handle(trust: ProxyTrust<'_>, req: Request, next: Next) -> Response {
    let trace_id = resolve_trace_id(trust, req.headers(), req.extensions());
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let started = Instant::now();

    let span = tracing::info_span!(
        "request",
        trace_id = %trace_id,
        method = %method,
        path = %path,
    );

    let mut response = scope(
        trace_id.clone(),
        async move {
            let response = next.run(req).await;
            tracing::debug!(
                status = response.status().as_u16(),
                latency_ms = started.elapsed().as_millis() as u64,
                "request completed"
            );
            response
        }
        .instrument(span),
    )
    .await;

    if let Ok(value) = HeaderValue::from_str(trace_id.as_str()) {
        response.headers_mut().insert(trace_header(), value);
    }
    response
}

/// 决定本次请求的 trace-id。
///
/// 仅当**直连对端**属于可信代理时继承上游 id，否则新生成：
/// trace-id 会进响应体、响应头与日志，若允许任意客户端指定，
/// 攻击者可用固定 id 覆盖/混淆自己的调用链。
fn resolve_trace_id(
    trust: ProxyTrust<'_>,
    headers: &axum::http::HeaderMap,
    extensions: &axum::http::Extensions,
) -> TraceId {
    if trust.enabled()
        && let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<SocketAddr>>()
        && trust.is_trusted(peer.ip())
        && let Some(inherited) = trace_id_from_headers(headers)
    {
        return inherited;
    }
    TraceId::new()
}
