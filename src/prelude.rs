//! 常用类型一键导入。
//!
//! ```
//! use aurora::prelude::*;
//! ```

pub use crate::app::{AppState, LifecycleError, LifecycleHook, LifecycleHooks, LifecycleReport};
#[cfg(feature = "auth")]
pub use crate::auth::{AuthMethod, AuthService, Identity, OptionalPrincipal, Principal};
pub use crate::boot::{StartupError, serve};
#[cfg(feature = "cache")]
pub use crate::cache::{CacheService, DistributedLock};
#[cfg(feature = "watch")]
pub use crate::config::ConfigWatcher;
pub use crate::config::{AppConfig, ConfigHandle, ConfigSource, Environment, preset};
#[cfg(feature = "db")]
pub use crate::db::{Cursor, DbPool, Repository, aurora_entity, prefix, transaction};
pub use crate::errors::{AppError, AppResult, ErrorCode};
#[cfg(feature = "geoip")]
pub use crate::geoip::{ClientGeo, GeoInfo, GeoIp};
pub use crate::http::{ApiResponse, AppRouter, AppRoutes, RequestContext, build_router};
#[cfg(feature = "i18n")]
pub use crate::i18n::{I18n, Locale, Translator};
#[cfg(feature = "openapi")]
pub use crate::openapi::OpenApiRouter;
#[cfg(feature = "queue")]
pub use crate::queue::{Consumer, Message, Producer, QueueService};
#[cfg(feature = "ratelimit")]
pub use crate::ratelimit::RateLimitService;
#[cfg(any(feature = "ws", feature = "sse"))]
pub use crate::realtime::{Hub, RealtimeMessage};
#[cfg(feature = "resilience")]
pub use crate::resilience::{Bulkhead, CircuitBreaker, Fallback, PolicyRegistry, RetryPolicy};
#[cfg(feature = "task")]
pub use crate::scheduler::{Job, JobSpec, Scheduler};
#[cfg(feature = "search")]
pub use crate::search::{
    Query as SearchQuery, SearchDocument, SearchEngine, SearchHit, SearchIndex,
};
#[cfg(feature = "security")]
pub use crate::security::{
    Argon2Params, Cipher, HmacSigner, Secret, SecurityContext, VerifyOutcome, hash_password,
    verify_password, verify_password_or_dummy,
};
pub use crate::telemetry::{TraceId, init_tracing};
#[cfg(feature = "template")]
pub use crate::template::{
    FsSource, MemorySource, Rendered, TemplateEngine, TemplateError, TemplateSource,
};
#[cfg(feature = "testing")]
pub use crate::testing::TestApp;
#[cfg(feature = "tls")]
pub use crate::tls::{CertStore, TlsService, serve_tls};
pub use crate::utils::Id;
pub use crate::utils::pagination::{CursorPage, Page};
pub use crate::utils::time::{now_unix_secs, now_utc};
#[cfg(feature = "security")]
pub use crate::validation::{
    Validated, ValidatedForm, ValidatedPath, ValidatedQuery, ValidationOptions, validate,
    validate_with,
};

pub use crate::axum::Router;
pub use crate::axum::routing::{delete, get, patch, post, put};
