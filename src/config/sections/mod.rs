//! 配置分段。
//!
//! 每个子系统一节；字段定义与默认值见 `docs/08-config-spec.md`。
//! 常开段：`app` / `server` / `telemetry` / `http`；其余段随对应 feature 落地。

mod app;
#[cfg(feature = "auth")]
pub(crate) mod auth;
#[cfg(feature = "cache")]
pub(crate) mod cache;
#[cfg(feature = "db")]
pub(crate) mod database;
pub(crate) mod de;
#[cfg(feature = "geoip")]
pub(crate) mod geoip;
mod http;
#[cfg(feature = "i18n")]
pub(crate) mod i18n;
#[cfg(feature = "nacos")]
pub(crate) mod nacos;
#[cfg(feature = "openapi")]
pub(crate) mod openapi;
#[cfg(feature = "queue")]
pub(crate) mod queue;
#[cfg(feature = "ratelimit")]
pub(crate) mod ratelimit;
#[cfg(any(feature = "ws", feature = "sse"))]
pub(crate) mod realtime;
#[cfg(feature = "resilience")]
pub(crate) mod resilience;
#[cfg(feature = "task")]
pub(crate) mod scheduler;
#[cfg(feature = "search")]
pub(crate) mod search;
#[cfg(feature = "security")]
pub(crate) mod security;
mod server;
mod telemetry;
#[cfg(feature = "template")]
pub(crate) mod template;
#[cfg(feature = "tls")]
pub(crate) mod tls;
#[cfg(feature = "security")]
mod validation;
#[cfg(feature = "watch")]
pub(crate) mod watch;

pub use app::AppSection;
#[cfg(feature = "auth")]
pub use auth::{AuthSection, RevocationSection};
#[cfg(feature = "authz")]
pub use auth::{AuthzAdapter, AuthzSection};
#[cfg(feature = "cache")]
pub use cache::{
    CacheBackend, CacheSection, LimitsSection, LockSection, RedisSection, SingleflightSection,
};
#[cfg(feature = "db")]
pub use database::DatabaseSection;
#[cfg(feature = "geoip")]
pub use geoip::GeoIpSection;
#[cfg(feature = "csrf")]
pub use http::CsrfSection;
#[cfg(feature = "idempotency")]
pub use http::IdempotencySection;
pub use http::{DefaultOffToggle, HttpSection, SecurityHeadersSection, ToggleSection};
#[cfg(feature = "ip-filter")]
pub use http::{IpFilterMode, IpFilterSection};
#[cfg(feature = "i18n")]
pub use i18n::I18nSection;
#[cfg(feature = "nacos")]
pub use nacos::{NacosFormat, NacosSection};
#[cfg(feature = "openapi")]
pub use openapi::OpenApiSection;
#[cfg(feature = "queue")]
pub use queue::{
    AmqpSection, DlqSection, KafkaSection, QueueBackendKind, QueueIdempotencySection,
    QueueRedisSection, QueueRetrySection, QueueSection,
};
#[cfg(feature = "ratelimit")]
pub use ratelimit::{RateLimitRule, RateLimitSection, RateLimitStore, RateLimitStrategy};
#[cfg(any(feature = "ws", feature = "sse"))]
pub use realtime::{InboundOverflow, RealtimeSection};
#[cfg(feature = "resilience")]
pub use resilience::{
    BulkheadSection, CircuitBreakerSection, ResilienceRetrySection, ResilienceSection,
    TimeoutSection,
};
#[cfg(feature = "task")]
pub use scheduler::{DistributedSection, LockErrorPolicy, SchedulerSection};
#[cfg(feature = "search")]
pub use search::{Analyzer, SearchSection};
#[cfg(feature = "security")]
pub use security::{
    CryptoAlgorithm, CryptoSecurity, HmacSecurity, MAX_ARGON2_MEMORY_KIB, MIN_ARGON2_MEMORY_KIB,
    MIN_ARGON2_TIME_COST, MIN_KEY_BYTES, PasswordSecurity, SecuritySection,
};
pub use server::{CorsSection, ServerSection};
pub use telemetry::{
    FileOverflow, FileRotation, FileSection, FileWriteMode, MetricsSection, OtelSection,
    TelemetrySection,
};
#[cfg(feature = "tls")]
pub use tls::{OcspSection, TlsSection};
#[cfg(feature = "template")]
pub use template::TemplateSection;
#[cfg(feature = "security")]
pub use validation::ValidationSection;
#[cfg(feature = "watch")]
pub use watch::WatchSection;

use serde::Deserialize;

/// 退避策略。
///
/// `[queue].retry`（**消费失败**重试）与 `[resilience].retry`（**出站**重试）共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetryBackoff {
    /// 固定退避。
    Fixed,
    /// 指数退避。
    Exponential,
}
