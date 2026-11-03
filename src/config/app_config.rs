//! 顶层配置（[`AppConfig`]）。
//!
//! 加载入口是 [`ConfigSource`](crate::config::ConfigSource)：**读哪些文件、按哪个环境跑，
//! 都由应用层显式给出**。框架不提供"默认配置目录"之类的隐式路径。

use serde::Deserialize;

use crate::boot::startup::StartupError;
use crate::config::Environment;
use crate::config::sections::*;

use super::ConfigHandle;

/// 顶层应用配置（强类型）。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
#[non_exhaustive]
pub struct AppConfig {
    /// 运行环境。
    ///
    /// **不来自配置文件**（`#[serde(skip)]`）：只由 `AURORA_ENV` 决定，并由
    /// 加载器在解析完成后写入。默认值是 [`Environment::Production`]（fail-closed）。
    ///
    /// 环境同时决定"读哪份 `config/{env}.*`"和"生产护栏是否生效"——把这两件事
    /// 绑在一个值上，就不存在"读的是 A 文件、护栏按 B 环境"的错位。
    #[serde(skip)]
    pub env: Environment,
    /// `[app]` 段。
    pub app: AppSection,
    /// `[server]` 段。
    pub server: ServerSection,
    /// `[telemetry]` 段。
    pub telemetry: TelemetrySection,
    /// `[http]` 段（中间件开关）。
    pub http: HttpSection,
    /// `[openapi]` 段（feature `openapi`）。
    #[cfg(feature = "openapi")]
    pub openapi: OpenApiSection,
    /// `[cache]` 段（feature `cache`）。
    #[cfg(feature = "cache")]
    pub cache: CacheSection,
    /// `[queue]` 段（feature `queue`）。
    #[cfg(feature = "queue")]
    pub queue: QueueSection,
    /// `[i18n]` 段（feature `i18n`）。
    #[cfg(feature = "i18n")]
    pub i18n: I18nSection,
    /// `[template]` 段（feature `template`）。
    #[cfg(feature = "template")]
    pub template: TemplateSection,
    /// `[geoip]` 段（feature `geoip`）。
    #[cfg(feature = "geoip")]
    pub geoip: GeoIpSection,
    /// `[realtime]` 段（feature `ws` / `sse`）。
    #[cfg(any(feature = "ws", feature = "sse"))]
    pub realtime: RealtimeSection,
    /// `[search]` 段（feature `search`）。
    #[cfg(feature = "search")]
    pub search: SearchSection,
    /// `[scheduler]` 段（feature `task`）。
    #[cfg(feature = "task")]
    pub scheduler: SchedulerSection,
    /// `[tls]` 段（feature `tls`）。
    #[cfg(feature = "tls")]
    pub tls: TlsSection,
    /// `[resilience]` 段（feature `resilience`）。
    #[cfg(feature = "resilience")]
    pub resilience: ResilienceSection,
    /// `[ratelimit]` 段（feature `ratelimit`）。
    #[cfg(feature = "ratelimit")]
    pub ratelimit: RateLimitSection,
    /// `[auth]` 段（feature `auth`）。
    #[cfg(feature = "auth")]
    pub auth: AuthSection,
    /// `[authz]` 段（feature `authz`）。
    #[cfg(feature = "authz")]
    pub authz: AuthzSection,
    /// `[security]` 段（feature `security`）。
    #[cfg(feature = "security")]
    pub security: SecuritySection,
    /// `[validation]` 段（feature `security`）。
    #[cfg(feature = "security")]
    pub validation: ValidationSection,
    /// `[database]` 段（feature `db`）。
    #[cfg(feature = "db")]
    pub database: DatabaseSection,
    /// `[nacos]` 段（feature `nacos`）。
    #[cfg(feature = "nacos")]
    pub nacos: NacosSection,
    /// `[watch]` 段（feature `watch`）。
    #[cfg(feature = "watch")]
    pub watch: WatchSection,
}

impl AppConfig {
    /// 启动期校验（fail-fast）。
    ///
    /// # Errors
    /// 端口为 0、监听地址非法、日志级别为空、安全段参数越界，或**生产环境**
    /// 的强制项（JWT 密钥 / secure Cookie / metrics 令牌）不满足时返回错误。
    pub fn validate(&self) -> Result<(), StartupError> {
        self.server.validate().map_err(StartupError::Config)?;
        if self.telemetry.level.trim().is_empty() {
            return Err(StartupError::Config(
                "telemetry.level must not be empty".to_string(),
            ));
        }
        // 生产护栏统一从这里取：**环境只有一个来源**（`AURORA_ENV` → `self.env`），
        // 配置文件里没有 `[app].env`，因此不存在"读的是生产文件、护栏却按开发跑"的错位。
        let production = self.env.is_production();

        // `allow_insecure` 是绊线而非开关：生产下不允许通过配置关闭护栏。
        // 想关只能改环境（并因此**同时**失去所有护栏），不存在"只关一条"的路径。
        if self.app.allow_insecure {
            if production {
                return Err(StartupError::Config(
                    "[app].allow_insecure = true 在生产环境被拒绝：\
                     生产护栏不允许通过配置关闭。若确需在无防护状态下运行，\
                     请显式设置 AURORA_ENV=development（此时**全部**护栏都会关闭）"
                        .to_string(),
                ));
            }
            tracing::warn!(
                "[app].allow_insecure = true：已显式承认本次运行使用不安全配置（仅限非生产）"
            );
        }

        // `[http]` 常开：中间件开关与配套参数的自洽性校验
        self.http
            .validate_for_env(production)
            .map_err(StartupError::Config)?;
        // CORS：非法 origin 不再被中间件静默丢弃，生产禁 `"*"`
        self.server
            .cors
            .validate_for_env(production)
            .map_err(StartupError::Config)?;
        self.telemetry
            .validate_for_env(production)
            .map_err(StartupError::Config)?;
        #[cfg(feature = "auth")]
        self.auth
            .validate_for_env(production)
            .map_err(StartupError::Config)?;
        #[cfg(feature = "security")]
        self.security.validate().map_err(StartupError::Config)?;
        // `[validation]`：`enabled = false` 拒绝启动（不允许关闭输入校验）
        #[cfg(feature = "security")]
        self.validation.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "db")]
        self.database.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "cache")]
        self.cache.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "queue")]
        self.queue.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "i18n")]
        self.i18n.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "template")]
        self.template.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "geoip")]
        self.geoip
            .validate_for_env(production)
            .map_err(StartupError::Config)?;
        #[cfg(any(feature = "ws", feature = "sse"))]
        self.realtime.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "search")]
        self.search.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "task")]
        {
            self.scheduler.validate().map_err(StartupError::Config)?;
            // 跨段组合校验（docs/02 §12.1）：选主锁必须能跨实例共享
            if self.scheduler.distributed.enabled && self.cache.backend == CacheBackend::Memory {
                return Err(StartupError::Config(
                    "[scheduler].distributed.enabled = true 需要共享的选主锁：\
                     请把 [cache].backend 设为 \"redis\"（memory 后端无法跨实例共享）"
                        .to_string(),
                ));
            }
        }
        #[cfg(feature = "ratelimit")]
        self.ratelimit.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "resilience")]
        self.resilience.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "tls")]
        self.tls.validate().map_err(StartupError::Config)?;
        #[cfg(feature = "nacos")]
        self.nacos
            .validate_for_env(production)
            .map_err(StartupError::Config)?;
        #[cfg(feature = "openapi")]
        self.openapi
            .validate_for_env(production)
            .map_err(StartupError::Config)?;
        Ok(())
    }

    /// 擦除配置里的密钥（内存清零）。
    ///
    /// 配置是 `Clone` 的、热更新会整份替换；旧快照被释放后，其密钥字符串仍留在
    /// 已释放的内存里（直到被覆写）。[`ConfigHandle::store`] 在替换后调用本方法，
    /// 把「已无人引用」的旧快照就地清零。
    ///
    /// 注意：这只覆盖「快照不再被引用」之后的清理；配置存活期间（以及 `Clone` 出来
    /// 的副本）密钥仍在内存中。
    pub fn zeroize_secrets(&mut self) {
        use zeroize::Zeroize as _;

        self.telemetry.metrics.token.zeroize();
        #[cfg(feature = "auth")]
        self.auth.jwt.secret.zeroize();
        #[cfg(feature = "security")]
        {
            self.security.crypto.key.zeroize();
            self.security.crypto.previous_key.zeroize();
            self.security.hmac.key.zeroize();
        }
        #[cfg(feature = "db")]
        self.database.url.zeroize();
        #[cfg(feature = "cache")]
        if let Some(url) = self.cache.redis.url.as_mut() {
            url.zeroize();
        }
        #[cfg(feature = "queue")]
        {
            if let Some(url) = self.queue.redis.url.as_mut() {
                url.zeroize();
            }
            if let Some(url) = self.queue.amqp.url.as_mut() {
                url.zeroize();
            }
        }
        #[cfg(feature = "nacos")]
        self.nacos.password.zeroize();
    }

    /// 包装为可共享的 [`ConfigHandle`]。
    #[must_use]
    pub fn into_handle(self) -> ConfigHandle {
        ConfigHandle::new(self)
    }

    /// 是否为生产环境。
    ///
    /// 等价于 `self.env.is_production()`：环境只有一个来源（`AURORA_ENV`），
    /// 所以这里不需要"取最严"之类的合并逻辑。
    #[must_use]
    pub fn is_production(&self) -> bool {
        self.env.is_production()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_swap_config_atomically() {
        let handle = ConfigHandle::new(AppConfig::default());
        assert_eq!(handle.load().server.port, 8080);

        let mut next = AppConfig::default();
        next.server.port = 9090;
        handle.store(next).expect("未改安全边界，应允许热更新");

        assert_eq!(handle.load().server.port, 9090, "store 后应读到新快照");
    }

    /// 非生产默认配置：显式声明 development，避免生产护栏干扰「测别的东西」的用例。
    fn dev_config() -> AppConfig {
        AppConfig {
            env: Environment::Development,
            ..AppConfig::default()
        }
    }

    #[cfg(feature = "resilience")]
    #[test]
    fn should_reject_zero_resilience_timeout() {
        let mut config = dev_config();
        // `[database]` 的校验排在 `[resilience]` 之前：启用 `db` 时必须先给连接串，
        // 否则这里拿到的是 database 的错误，测不到 resilience。
        #[cfg(feature = "db")]
        {
            config.database.url = "sqlite::memory:".to_string();
        }
        config.resilience.timeout.default = std::time::Duration::ZERO;
        let error = config.validate().unwrap_err();
        assert!(
            format!("{error}").contains("timeout.default"),
            "出站超时为 0 等于关闭超时，必须拒绝启动：{error}"
        );
    }

    #[cfg(feature = "resilience")]
    #[test]
    fn should_reject_unbounded_retry() {
        let mut config = dev_config();
        config.resilience.retry.max_attempts = 100;
        assert!(
            config.validate().is_err(),
            "无上限的重试会把下游故障放大成雪崩"
        );
    }

    #[test]
    fn should_share_snapshot_across_clones() {
        let handle = ConfigHandle::new(AppConfig::default());
        let clone = handle.clone();

        let mut next = AppConfig::default();
        next.app.name = "renamed".to_string();
        handle.store(next).expect("未改安全边界，应允许热更新");

        assert_eq!(clone.load().app.name, "renamed", "克隆共享同一 ArcSwap");
    }

    /// 仓库里的 `config/development.toml` 必须能反序列化成 `AppConfig`。
    ///
    /// 防的是「加了字段但忘了同步默认配置」这类漂移（示例/部署都读这份文件）。
    #[cfg(feature = "tls")]
    #[test]
    fn should_parse_shipped_development_config() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/config/development.toml");
        let config: AppConfig = ::config::Config::builder()
            .add_source(::config::File::new(path, ::config::FileFormat::Toml))
            .build()
            .and_then(::config::Config::try_deserialize)
            .expect("config/development.toml 应能反序列化为 AppConfig");

        assert!(!config.tls.enabled, "默认不启用 HTTPS");
        assert!(!config.tls.ocsp.enabled, "默认不启用 OCSP 装订");
        assert_eq!(config.tls.ocsp.refresh_interval.as_secs(), 900);
    }

    #[test]
    fn should_use_defaults_when_no_sources() {
        let cfg = dev_config();
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.server.host, "0.0.0.0");

        // 启用 `db` 时 `database.url` 必填，故默认配置校验必然失败。
        #[cfg(not(feature = "db"))]
        cfg.validate().unwrap();
        #[cfg(feature = "db")]
        assert!(
            cfg.validate().is_err(),
            "启用 db 时缺少 database.url 应校验失败"
        );
    }

    /// 回归：**默认环境必须是生产**（fail-closed）。
    ///
    /// 修复前 `AppSection::default()` 的 `env` 是 `"development"`，于是"忘了配环境"
    /// 得到的是最松的一档：JWT 密钥可为空、CORS 可 `*`、`/metrics` 可无鉴权。
    #[test]
    fn should_default_to_production() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.env, Environment::Production, "默认环境必须是生产");
        assert!(cfg.is_production(), "默认必须按生产处理");

        // 生产护栏生效：JWT 密钥为空直接拒绝启动（启用 auth 时）
        #[cfg(feature = "auth")]
        {
            let error = cfg.validate().unwrap_err();
            assert!(
                format!("{error}").contains("jwt.secret"),
                "默认环境时生产护栏必须生效，实际：{error}"
            );
        }

        // 显式 development 才放开
        assert!(!dev_config().is_production());
    }

    /// 回归：`allow_insecure` 是绊线，不是开关。
    #[test]
    fn should_reject_allow_insecure_in_production() {
        let mut config = AppConfig::default(); // 默认 → 生产
        config.app.allow_insecure = true;
        let error = config.validate().unwrap_err();
        assert!(
            format!("{error}").contains("allow_insecure"),
            "生产环境不允许通过配置关闭护栏，实际：{error}"
        );
    }

    #[test]
    fn should_reject_port_zero() {
        let cfg = AppConfig {
            server: ServerSection {
                port: 0,
                ..ServerSection::default()
            },
            ..dev_config()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn should_zeroize_secrets() {
        let mut config = AppConfig::default();
        config.telemetry.metrics.token = "scrape-token".to_string();
        #[cfg(feature = "auth")]
        {
            config.auth.jwt.secret = "jwt-secret-material".to_string();
        }

        config.zeroize_secrets();

        assert!(config.telemetry.metrics.token.is_empty(), "token 应被清零");
        #[cfg(feature = "auth")]
        assert!(config.auth.jwt.secret.is_empty(), "JWT 密钥应被清零");
    }

    #[test]
    fn should_deref_through_handle() {
        let handle = AppConfig::default().into_handle();
        assert_eq!(handle.load().server.port, 8080);
    }
}
