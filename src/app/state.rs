//! 应用状态。
//!
//! `AppState` 以 `Arc` 共享，经 `State<AppState>` 注入 handler。可选能力字段
//! 用 `#[cfg(feature = ...)]` 门控——未启用即字段不存在（**编译期报错**，
//! 不用 `Option`）。

// `FromRef` 仅在有可选能力子状态（feature 门控）时被使用；
// 新增可选能力模块时需同步扩展此 cfg 条件。
#[cfg(any(
    feature = "auth",
    feature = "db",
    feature = "cache",
    feature = "queue",
    feature = "ratelimit",
    feature = "resilience",
    feature = "i18n",
    feature = "geoip",
    feature = "task",
    feature = "security",
    feature = "template",
    feature = "metrics"
))]
use axum::extract::FromRef;

use crate::app::lifecycle::LifecycleHooks;
#[cfg(feature = "auth")]
use crate::auth::AuthService;
#[cfg(feature = "cache")]
use crate::cache::CacheService;
use crate::config::ConfigHandle;
#[cfg(feature = "db")]
use crate::db::DbPool;
#[cfg(feature = "geoip")]
use crate::geoip::GeoIp;
#[cfg(feature = "i18n")]
use crate::i18n::I18n;
#[cfg(feature = "queue")]
use crate::queue::QueueService;
#[cfg(feature = "ratelimit")]
use crate::ratelimit::RateLimitService;
#[cfg(feature = "resilience")]
use crate::resilience::PolicyRegistry;
#[cfg(feature = "task")]
use crate::scheduler::Scheduler;
#[cfg(feature = "security")]
use crate::security::SecurityContext;
use crate::telemetry::HealthRegistry;
#[cfg(feature = "metrics")]
use crate::telemetry::Metrics;
#[cfg(feature = "template")]
use crate::template::TemplateEngine;

/// 全局应用状态。
#[derive(Clone, Debug)]
pub struct AppState {
    /// 配置句柄（常开）。
    pub config: ConfigHandle,
    /// 就绪探针注册表（常开；可在装配后继续注册）。
    pub health: HealthRegistry,
    /// 生命周期钩子（常开）：`run_startup` 在监听前、`run_shutdown` 在服务停止后
    /// 由应用显式执行（顺序见 `docs/02` §3）。
    pub lifecycle: LifecycleHooks,
    /// 认证授权服务（feature `auth`）。
    #[cfg(feature = "auth")]
    pub auth: AuthService,
    /// 缓存与分布式锁（feature `cache`）。
    #[cfg(feature = "cache")]
    pub cache: CacheService,
    /// 队列（feature `queue`）。
    #[cfg(feature = "queue")]
    pub queue: QueueService,
    /// 国际化（feature `i18n`）。
    #[cfg(feature = "i18n")]
    pub i18n: I18n,
    /// 模板引擎（feature `template`）。
    ///
    /// 启动期已全量加载并编译（任一模板语法错误即拒绝启动）。渲染失败一律映射为
    /// `50008` 的**统一 JSON 错误体**，不提供 HTML 错误页（`docs/20` §1.2）。
    #[cfg(feature = "template")]
    pub template: TemplateEngine,
    /// IP 地理定位（feature `geoip`）。
    #[cfg(feature = "geoip")]
    pub geoip: GeoIp,
    /// 任务调度（feature `task`）。
    #[cfg(feature = "task")]
    pub scheduler: Scheduler,
    /// 指标集合（feature `metrics`）。
    #[cfg(feature = "metrics")]
    pub metrics: Metrics,
    /// 出站治理策略（feature `resilience`）。
    #[cfg(feature = "resilience")]
    pub resilience: PolicyRegistry,
    /// 限流（feature `ratelimit`）。
    #[cfg(feature = "ratelimit")]
    pub rate_limit: RateLimitService,
    /// 安全能力：对称加密器 / HMAC / Argon2 参数（feature `security`）。
    ///
    /// 密钥留空时对应能力为 `None`，取用入口（`security.cipher()` /
    /// `security.hmac()`）会报错而不是降级成空密钥。
    #[cfg(feature = "security")]
    pub security: SecurityContext,
    /// 数据库连接池（feature `db`）。
    #[cfg(feature = "db")]
    pub db: DbPool,
    /// 配置热更新器（feature `watch`，`[watch].enabled = true` 时存在）。
    ///
    /// 持有它是为了让监听任务存活——`ConfigWatcher` 的 `Drop` 会 abort 任务，
    /// 若只放在局部变量里，函数返回后热更新立刻停止。
    #[cfg(feature = "watch")]
    pub config_watcher: Option<std::sync::Arc<crate::config::ConfigWatcher>>,
    /// 模板目录监听器（feature `template-hot-reload`，`[template].hot_reload = true` 时存在）。
    ///
    /// 同 `config_watcher`：持有它只是为了「让监听任务活着」，drop 即停止监听。
    #[cfg(all(feature = "template", feature = "template-hot-reload"))]
    pub template_watcher: Option<std::sync::Arc<crate::template::TemplateWatcher>>,
}

/// 子状态拆分：`State<RateLimitService>` 可直接注入 handler。
#[cfg(feature = "ratelimit")]
impl FromRef<AppState> for RateLimitService {
    fn from_ref(state: &AppState) -> Self {
        state.rate_limit.clone()
    }
}

/// 子状态拆分：`State<PolicyRegistry>` 可直接注入 handler。
#[cfg(feature = "resilience")]
impl FromRef<AppState> for PolicyRegistry {
    fn from_ref(state: &AppState) -> Self {
        state.resilience.clone()
    }
}

/// 子状态拆分：`State<Metrics>` 可直接注入 handler。
#[cfg(feature = "metrics")]
impl FromRef<AppState> for Metrics {
    fn from_ref(state: &AppState) -> Self {
        state.metrics.clone()
    }
}

/// 子状态拆分：`State<Scheduler>` 可直接注入 handler。
#[cfg(feature = "task")]
impl FromRef<AppState> for Scheduler {
    fn from_ref(state: &AppState) -> Self {
        state.scheduler.clone()
    }
}

/// 子状态拆分：`State<GeoIp>` 可直接注入 handler。
#[cfg(feature = "geoip")]
impl FromRef<AppState> for GeoIp {
    fn from_ref(state: &AppState) -> Self {
        state.geoip.clone()
    }
}

/// 子状态拆分：`State<I18n>` 可直接注入 handler。
#[cfg(feature = "i18n")]
impl FromRef<AppState> for I18n {
    fn from_ref(state: &AppState) -> Self {
        state.i18n.clone()
    }
}

/// 子状态拆分：`State<QueueService>` 可直接注入 handler。
#[cfg(feature = "queue")]
impl FromRef<AppState> for QueueService {
    fn from_ref(state: &AppState) -> Self {
        state.queue.clone()
    }
}

/// 子状态拆分：`State<CacheService>` 可直接注入 handler。
#[cfg(feature = "cache")]
impl FromRef<AppState> for CacheService {
    fn from_ref(state: &AppState) -> Self {
        state.cache.clone()
    }
}

/// 子状态拆分：`State<DbPool>` 可直接注入 handler（见 `docs/02` §5）。
#[cfg(feature = "db")]
impl FromRef<AppState> for DbPool {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

/// 子状态拆分：`State<AuthService>` 可直接注入 handler。
#[cfg(feature = "auth")]
impl FromRef<AppState> for AuthService {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

/// 子状态拆分：`State<SecurityContext>` 可直接注入 handler。
#[cfg(feature = "security")]
impl FromRef<AppState> for SecurityContext {
    fn from_ref(state: &AppState) -> Self {
        state.security.clone()
    }
}

/// 子状态拆分：`State<TemplateEngine>` 可直接注入 handler。
#[cfg(feature = "template")]
impl FromRef<AppState> for TemplateEngine {
    fn from_ref(state: &AppState) -> Self {
        state.template.clone()
    }
}
