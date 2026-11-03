//! aurora —— 以 Web 为主要方向的 Rust 应用基座（Application Base）。
//!
//! 形态为**主 crate + 过程宏 crate**（`macros/`，见 [`macros`] 与 `docs/03` §7）
//! 的模块化能力集合，技术栈为 Axum + Tokio + Tower。图省事只想依赖一个 crate
//! 时用 `aurora` 即可——过程宏经 `aurora::db::aurora_entity` 转发。
//!
//! # 模块概览（M0 核心）
//!
//! - [`app`]：应用装配（[`AppState`](app::AppState)）
//! - [`boot`]：启动状态机与优雅关闭（[`serve`](boot::serve)、[`StartupError`](boot::StartupError)）
//! - [`cache`]：缓存与分布式锁（feature `cache`；[`CacheService`](cache::CacheService)）
//! - [`config`]：分层配置（[`AppConfig`](config::AppConfig)、[`ConfigHandle`](config::ConfigHandle)）
//! - [`errors`]：统一错误与错误码（[`AppError`](errors::AppError)、[`ErrorCode`](errors::ErrorCode)）
//! - [`http`]：Router 与中间件栈（[`build_router`](http::build_router)、[`ApiResponse`](http::ApiResponse)）
//! - [`openapi`]：OpenAPI 文档装配（feature `openapi`；[`OpenApiRouter`](openapi::OpenApiRouter)）
//! - [`telemetry`]：日志 / 追踪 / 健康检查（[`init_tracing`](telemetry::init_tracing)）
//! - [`template`]：模板渲染（feature `template`；[`TemplateEngine`](template::TemplateEngine)）
//! - [`utils`]：通用工具（时间、ID、分页、客户端 IP）
//! - [`macros`]：声明宏
//!
//! 常用类型可经 `use aurora::prelude::*;` 一键导入。
//!
//! # 快速开始
//!
//! ```no_run
//! use aurora::prelude::*;
//!
//! # async fn run() -> anyhow::Result<()> {
//! let config = preset::load_env_var("AURORA_ENV", "config")?;
//! init_tracing(&config.telemetry)?;
//! let state = AppState::build(&config).await?;
//! let routes = Router::new().route("/", get(|| async { ApiResponse::ok("hello") }));
//! let app = build_router(routes, state);
//! serve(app, config.server.socket_addr()?).await?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// 过程宏 `AuroraEntity` 生成的是 `::aurora::db::...` 绝对路径：给 crate 自身起个别名，
// 否则 crate 内部的单元测试 / 文档示例里解析不到 `::aurora`。
extern crate self as aurora;

pub mod app;
#[cfg(feature = "auth")]
pub mod auth;
pub mod boot;
#[cfg(feature = "cache")]
pub mod cache;
pub mod config;
#[cfg(feature = "db")]
pub mod db;
pub mod errors;
#[cfg(feature = "geoip")]
pub mod geoip;
pub mod http;
#[cfg(feature = "i18n")]
pub mod i18n;
pub mod macros;
#[cfg(feature = "openapi")]
pub mod openapi;
pub mod prelude;
#[cfg(feature = "queue")]
pub mod queue;
#[cfg(feature = "ratelimit")]
pub mod ratelimit;
#[cfg(any(feature = "ws", feature = "sse"))]
pub mod realtime;
#[cfg(feature = "resilience")]
pub mod resilience;
#[cfg(feature = "task")]
pub mod scheduler;
#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "security")]
pub mod security;
pub mod telemetry;
#[cfg(feature = "template")]
pub mod template;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "tls")]
pub mod tls;
pub mod utils;
#[cfg(feature = "security")]
pub mod validation;

// —— 依赖转发：使下游通常只需依赖 `aurora` 一个 crate ——
// 注：`http` crate 的转发被本 crate 的同名模块 `http` 遮蔽，故此处不转发；
// 其类型可经 `aurora::axum::http` 访问。
pub use axum;
pub use chrono;
pub use serde;
pub use serde_json;
pub use tokio;
pub use tower;
pub use tower_http;
pub use tracing;
#[cfg(feature = "openapi")]
pub use utoipa;
