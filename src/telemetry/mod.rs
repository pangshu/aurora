//! 可观测性模块。
//!
//! 统一提供日志、指标、追踪与健康检查（`logger` 已并入本模块）。
//! M1 落地日志初始化、trace-id 与健康检查；M6 落地指标（feature `metrics`）与
//! 追踪导出（feature `otel`）。

#[cfg(feature = "log-file")]
mod appender;
pub(crate) mod format;
pub(crate) mod health;
mod json_format;
mod layer;
mod logging;
#[cfg(feature = "metrics")]
pub(crate) mod metrics;
mod spawn;
mod timer;
#[cfg(feature = "otel")]
pub(crate) mod trace;

pub use format::LogFormat;
pub use health::{
    CheckOutcome, DEFAULT_CACHE_TTL, FnHealth, HealthCheck, HealthRegistry, HealthStatus,
    probe_failure,
};
pub use layer::{
    InvalidTraceId, TRACE_ID_HEADER, TraceId, UNKNOWN_TRACE_ID, current_trace_id, new_trace_id,
    scope, trace_id_from_headers, try_current_trace_id,
};
pub use spawn::{spawn, spawn_in_trace};
#[cfg(feature = "metrics")]
pub use metrics::Metrics;

/// 运行期调整日志级别（`[telemetry].level` 热更新）。
///
/// 表达式与 `RUST_LOG` 一致（如 `info`、`aurora=debug,tower_http=warn`）；
/// 过滤器包在 `reload` 层里，替换后**立即生效**，不需要重启。
///
/// # Errors
/// 表达式非法，或日志订阅器尚未初始化时返回错误说明。
pub use logging::set_level;

/// 刷新并关闭日志/追踪后端（**进程退出前调用**）。
///
/// 顺序：先 OTel（flush 未发送的 span / 日志），再文件日志（排空本地写队列）——
/// 这样「otel 已关闭」这类收尾日志也能落到文件。未启用对应 feature 时为空操作。
///
/// 由 `boot::serve` / `tls::serve` 在服务退出后调用；**不经 `serve` 的应用**
/// （CLI、一次性任务）必须自己调一次，否则 `[telemetry].file` 的异步队列会丢尾。
pub fn shutdown() {
    #[cfg(feature = "otel")]
    trace::shutdown();
    #[cfg(feature = "log-file")]
    appender::shutdown(std::time::Duration::from_secs(5));
}

/// 初始化日志、指标与追踪（实现见 `logging`）。
pub use logging::init_tracing;
