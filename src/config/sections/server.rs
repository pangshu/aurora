//! `[server]` 配置段。

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use byte_unit::Byte;
use serde::Deserialize;

use crate::boot::startup::StartupError;
use crate::config::sections::de::{de_byte, de_duration};

/// 请求体上限的下界（1 KiB）：再小等于拒绝一切带 body 的请求。
pub const MIN_BODY_LIMIT_BYTES: u64 = 1024;

/// 请求体上限的上界（256 MiB）：防止把 `body_limit` 写成天文数字后变相关闭限制。
pub const MAX_BODY_LIMIT_BYTES: u64 = 256 * 1024 * 1024;

/// 超时豁免路径的数量上限。
pub const MAX_TIMEOUT_EXEMPT_PATHS: usize = 32;

/// 服务端配置段。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ServerSection {
    /// 监听地址。
    pub host: String,
    /// 监听端口。
    pub port: u16,
    /// 应用时区（IANA 名，如 `Asia/Shanghai`）。
    ///
    /// **空串 = 跟随系统时区**（[`chrono::Local`]，含 `TZ` 环境变量）。它同时决定：
    /// 日志时间戳、写库的墙上时间（`create_time` 等 `datetime` 列）、读库还原绝对时刻、
    /// MySQL 会话时区（`DEFAULT CURRENT_TIMESTAMP` 依赖它）。
    ///
    /// 进程级设置，**不可热更新**：运行期改配置不会生效，变更需重启；对已有数据而言，
    /// 改时区等价于一次数据平移（见应用侧 `tz_shift` 脚本）。
    pub timezone: String,
    /// Tokio worker 线程数；缺省（`None`）表示使用 CPU 核数。
    pub worker_threads: Option<u32>,
    /// **普通请求**总超时（入站）；SSE / WS 豁免，由 `[realtime].heartbeat_timeout` 治理。
    #[serde(deserialize_with = "de_duration")]
    pub request_timeout: Duration,
    /// 豁免超时的路径前缀（长连接 / 流式路由）。
    pub timeout_exempt_paths: Vec<String>,
    /// 优雅关闭等待上限。
    #[serde(deserialize_with = "de_duration")]
    pub graceful_shutdown_timeout: Duration,
    /// 请求体大小上限。
    #[serde(deserialize_with = "de_byte")]
    pub body_limit: Byte,
    /// 是否信任反向代理传入的 `X-Forwarded-*` 头。
    ///
    /// **必须同时配置 [`trusted_proxies`](Self::trusted_proxies)**：只有直连对端
    /// 落在可信网段内时才会解析转发头，否则一律用 TCP 对端地址。
    pub trust_proxy: bool,
    /// 可信反向代理网段（CIDR，如 `["10.0.0.0/8", "127.0.0.1/32"]`）。
    ///
    /// `trust_proxy = true` 时必填——否则任何直连客户端都能伪造
    /// `X-Forwarded-For` 绕过 `ip_filter` 白名单与按 IP 限流。
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// CORS 配置。
    pub cors: CorsSection,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            timezone: String::new(),
            worker_threads: None,
            request_timeout: Duration::from_secs(10),
            timeout_exempt_paths: Vec::new(),
            graceful_shutdown_timeout: Duration::from_secs(10),
            body_limit: Byte::from_u64(2 * 1024 * 1024),
            trust_proxy: false,
            trusted_proxies: Vec::new(),
            cors: CorsSection::default(),
        }
    }
}

impl ServerSection {
    /// 启动期校验（fail-fast）。
    ///
    /// # Errors
    /// 端口为 0、`host` 不是合法 IP 字面量，或 `trust_proxy = true` 但
    /// `trusted_proxies` 为空时返回错误。
    pub fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("server.port must not be 0".to_string());
        }
        if self.host.parse::<IpAddr>().is_err() {
            return Err(format!("invalid server.host `{}`", self.host));
        }
        let timezone = self.timezone.trim();
        if !timezone.is_empty() && crate::utils::time::parse_timezone(timezone).is_err() {
            return Err(format!(
                "[server].timezone = \"{timezone}\" 不是合法的 IANA 时区\
                 （如 Asia/Shanghai；留空表示跟随系统时区）"
            ));
        }
        if self.trust_proxy && self.trusted_proxies.is_empty() {
            return Err("[server].trust_proxy = true 但 trusted_proxies 为空：\
                 任何直连客户端都能伪造 X-Forwarded-For，绕过 IP 白名单与按 IP 限流。\
                 请填入代理所在网段（如 [\"10.0.0.0/8\"]），或把 trust_proxy 设为 false"
                .to_string());
        }
        // `/0` 与「空列表」语义等价（都等于信任任何直连客户端），是上面那条校验的绕过路径：
        // 把 `[]` 换成 `["0.0.0.0/0"]` 就能让校验通过、防护失效。无论 `trust_proxy`
        // 当前是否开启都拒绝——这个值没有任何合法用途，留着迟早被人打开开关时误用。
        if let Some(net) = self
            .trusted_proxies
            .iter()
            .find(|net| net.prefix_len() == 0)
        {
            return Err(format!(
                "[server].trusted_proxies 不能包含 `{net}`：它覆盖全部地址，\
                 等价于信任任何直连客户端——X-Forwarded-For 可被任意伪造，\
                 IP 白名单与按 IP 限流形同虚设。请填入代理实际所在网段（如 [\"10.0.0.0/8\"]）"
            ));
        }
        // 超时：0 等于「立即超时」，几乎总是误配
        if self.request_timeout.is_zero() {
            return Err("[server].request_timeout 必须大于 0".to_string());
        }
        if self.graceful_shutdown_timeout.is_zero() {
            return Err("[server].graceful_shutdown_timeout 必须大于 0".to_string());
        }
        // 请求体上限：太小事事被拒，太大等于没限制
        let body = self.body_limit.as_u64();
        if body < MIN_BODY_LIMIT_BYTES {
            return Err(format!(
                "[server].body_limit = {body} 字节过小（至少 {MIN_BODY_LIMIT_BYTES} 字节）"
            ));
        }
        if body > MAX_BODY_LIMIT_BYTES {
            return Err(format!(
                "[server].body_limit = {body} 字节过大（上限 {MAX_BODY_LIMIT_BYTES} 字节，\
                 大文件请走对象存储 / 流式上传）"
            ));
        }
        // 超时豁免路径：必须是以 `/` 开头的具体前缀，不能是根路径
        if self.timeout_exempt_paths.len() > MAX_TIMEOUT_EXEMPT_PATHS {
            return Err(format!(
                "[server].timeout_exempt_paths 最多 {MAX_TIMEOUT_EXEMPT_PATHS} 条"
            ));
        }
        for path in &self.timeout_exempt_paths {
            let path = path.trim();
            if path.is_empty() || path == "/" {
                return Err(
                    "[server].timeout_exempt_paths 不允许空串或 \"/\"（那会让全站豁免超时，\
                     慢速连接可直接拖垮进程）"
                        .to_string(),
                );
            }
            if !path.starts_with('/') {
                return Err(format!(
                    "[server].timeout_exempt_paths 中的 \"{path}\" 必须以 '/' 开头"
                ));
            }
        }
        Ok(())
    }

    /// 由 `host` / `port` 解析出监听地址。
    ///
    /// # Errors
    /// 当 `host` 不是合法 IP 字面量时返回 [`StartupError::Config`]。
    pub fn socket_addr(&self) -> Result<SocketAddr, StartupError> {
        let ip: IpAddr = self
            .host
            .parse()
            .map_err(|_| StartupError::Config(format!("invalid server.host `{}`", self.host)))?;
        Ok(SocketAddr::new(ip, self.port))
    }

    /// 请求体上限的字节数。
    #[must_use]
    pub fn body_limit_bytes(&self) -> usize {
        usize::try_from(self.body_limit.as_u64()).unwrap_or(usize::MAX)
    }
}

/// CORS 配置段。默认策略收紧（不放开任何跨域来源）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct CorsSection {
    /// 允许的来源；含 `"*"` 表示任意来源；空表示不放开跨域。
    pub allowed_origins: Vec<String>,
    /// 允许的方法。
    pub allowed_methods: Vec<String>,
    /// 允许的请求头。
    pub allowed_headers: Vec<String>,
    /// 预检结果缓存时长。
    #[serde(deserialize_with = "de_duration")]
    pub max_age: Duration,
}

impl Default for CorsSection {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allowed_methods: vec!["GET".to_string(), "POST".to_string()],
            allowed_headers: vec!["Content-Type".to_string(), "Authorization".to_string()],
            max_age: Duration::from_secs(3600),
        }
    }
}

impl CorsSection {
    /// 启动期校验（按**非生产**环境处理，等价于 `validate_for_env(false)`）。
    ///
    /// # Errors
    /// origin / method / header 非法时返回错误。
    pub fn validate(&self) -> Result<(), String> {
        self.validate_for_env(false)
    }

    /// 按运行环境校验（fail-fast）。
    ///
    /// 非法 origin 此前会被中间件 `filter_map(...ok())` **静默丢弃**——线上表现是
    /// 「配了跨域但不生效」，排查时毫无线索。这里改为启动期报错。
    ///
    /// # Errors
    /// 生产环境使用 `"*"`、origin 带路径 / 尾部斜杠、方法或请求头名非法时返回错误。
    pub fn validate_for_env(&self, production: bool) -> Result<(), String> {
        for origin in &self.allowed_origins {
            validate_origin(origin)?;
        }
        for method in &self.allowed_methods {
            let method = method.trim();
            if method == "*" {
                return Err(
                    "[server].cors.allowed_methods 不允许 \"*\"（等于放开所有方法，\
                     含 DELETE / PATCH）"
                        .to_string(),
                );
            }
            let parsed = method.parse::<http::Method>().map_err(|_| {
                format!("[server].cors.allowed_methods 中的 \"{method}\" 不是合法 HTTP 方法")
            })?;
            if parsed == http::Method::TRACE || parsed == http::Method::CONNECT {
                return Err(format!(
                    "[server].cors.allowed_methods 不允许 {method}（TRACE / CONNECT 不应跨域开放）"
                ));
            }
        }
        for header in &self.allowed_headers {
            let header = header.trim();
            if header == "*" {
                continue;
            }
            http::header::HeaderName::from_bytes(header.as_bytes()).map_err(|_| {
                format!("[server].cors.allowed_headers 中的 \"{header}\" 不是合法请求头名")
            })?;
        }
        if production && self.allowed_origins.iter().any(|o| o.trim() == "*") {
            return Err("[server].cors.allowed_origins 含 \"*\"，在生产环境被拒绝：\
                 任意站点都能跨域读取接口响应。请显式列出允许的来源"
                .to_string());
        }
        Ok(())
    }
}

/// 校验 CORS origin：`*` 或 `scheme://host[:port]`。
///
/// 浏览器发出的 `Origin` **不带路径、不带尾部斜杠**；`https://a.com/` 这种写法
/// 永远匹配不上，必须在校验期拦下来。
fn validate_origin(origin: &str) -> Result<(), String> {
    let origin = origin.trim();
    if origin == "*" {
        return Ok(());
    }
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .ok_or_else(|| {
            format!(
                "[server].cors.allowed_origins 中的 \"{origin}\" 必须以 http:// 或 https:// 开头"
            )
        })?;
    if rest.contains('/') {
        return Err(format!(
            "[server].cors.allowed_origins 中的 \"{origin}\" 不能含路径或尾部斜杠\
             （浏览器的 Origin 不带路径，带斜杠永远匹配不上）"
        ));
    }
    if rest.contains(['?', '#', '@']) || rest.contains(char::is_whitespace) {
        return Err(format!(
            "[server].cors.allowed_origins 中的 \"{origin}\" 含非法字符（只允许 scheme://host[:port]）"
        ));
    }
    let (host, port) = split_host_port(rest).ok_or_else(|| {
        format!("[server].cors.allowed_origins 中的 \"{origin}\" 不是合法的 host[:port]")
    })?;
    if host.is_empty() {
        return Err(format!(
            "[server].cors.allowed_origins 中的 \"{origin}\" 缺少主机名"
        ));
    }
    if let Some(port) = port {
        port.parse::<u16>()
            .map_err(|_| format!("[server].cors.allowed_origins 中的 \"{origin}\" 端口非法"))?;
    }
    Ok(())
}

/// 拆分 `host[:port]`（支持 `[::1]:8080`）。
fn split_host_port(rest: &str) -> Option<(&str, Option<&str>)> {
    if let Some(after) = rest.strip_prefix('[') {
        let (host, tail) = after.split_once(']')?;
        return match tail.strip_prefix(':') {
            Some(port) => Some((host, Some(port))),
            None if tail.is_empty() => Some((host, None)),
            None => None,
        };
    }
    match rest.rsplit_once(':') {
        Some((host, port)) => Some((host, Some(port))),
        None => Some((rest, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_default_socket_addr() {
        let s = ServerSection::default();
        assert_eq!(s.socket_addr().unwrap(), "0.0.0.0:8080".parse().unwrap());
    }

    #[test]
    fn should_error_when_host_invalid() {
        let s = ServerSection {
            host: "not-an-ip".to_string(),
            ..ServerSection::default()
        };
        assert!(s.socket_addr().is_err());
    }

    #[test]
    fn should_parse_duration_and_size_from_strings() {
        let json = r#"
            {
                "host": "127.0.0.1",
                "port": 9000,
                "request_timeout": "30s",
                "body_limit": "4MB",
                "graceful_shutdown_timeout": "5s",
                "cors": { "max_age": "10m" }
            }
        "#;
        let s: ServerSection = serde_json::from_str(json).unwrap();
        assert_eq!(s.request_timeout, Duration::from_secs(30));
        assert_eq!(s.body_limit.as_u64(), 4 * 1000 * 1000);
        assert_eq!(s.graceful_shutdown_timeout, Duration::from_secs(5));
        assert_eq!(s.cors.max_age, Duration::from_secs(600));
    }

    #[test]
    fn should_parse_duration_and_size_from_integers() {
        let json = r#"{ "request_timeout": 5, "body_limit": 1024 }"#;
        let s: ServerSection = serde_json::from_str(json).unwrap();
        assert_eq!(s.request_timeout, Duration::from_secs(5));
        assert_eq!(s.body_limit.as_u64(), 1024);
    }

    #[test]
    fn should_default_worker_threads_to_none() {
        let s = ServerSection::default();
        assert!(s.worker_threads.is_none());
        assert!(s.timeout_exempt_paths.is_empty());
    }

    #[test]
    fn should_require_trusted_proxies_when_trusting_proxy_headers() {
        let s = ServerSection {
            trust_proxy: true,
            ..ServerSection::default()
        };
        let error = s.validate().unwrap_err();
        assert!(error.contains("trusted_proxies"), "实际：{error}");

        let ok = ServerSection {
            trust_proxy: true,
            trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
            ..ServerSection::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn should_reject_global_trusted_network() {
        // `0.0.0.0/0` 与空列表等价：都等于信任任何直连客户端
        let v4 = ServerSection {
            trust_proxy: true,
            trusted_proxies: vec!["0.0.0.0/0".parse().unwrap()],
            ..ServerSection::default()
        };
        let error = v4.validate().unwrap_err();
        assert!(error.contains("0.0.0.0/0"), "实际：{error}");

        let v6 = ServerSection {
            trust_proxy: true,
            trusted_proxies: vec!["::/0".parse().unwrap()],
            ..ServerSection::default()
        };
        assert!(v6.validate().is_err(), "IPv6 全零网段同样必须拒绝");

        // 即使 `trust_proxy = false`（列表当前不生效）也拒绝：这个值没有合法用途
        let inert = ServerSection {
            trust_proxy: false,
            trusted_proxies: vec!["0.0.0.0/0".parse().unwrap()],
            ..ServerSection::default()
        };
        assert!(inert.validate().is_err(), "不生效的全零网段同样必须拒绝");

        // 正常网段不受影响
        let ok = ServerSection {
            trust_proxy: true,
            trusted_proxies: vec!["10.0.0.0/8".parse().unwrap(), "::1/128".parse().unwrap()],
            ..ServerSection::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn should_reject_unknown_keys() {
        // `trust_prox` 少个 y：以前被静默忽略（值仍是 false，看起来"安全"，
        // 但运维以为已经配上了）；现在直接报错。
        let parsed: Result<ServerSection, _> = serde_json::from_str(r#"{ "trust_prox": true }"#);
        assert!(parsed.is_err(), "未知键必须报错：{parsed:?}");

        let parsed: Result<CorsSection, _> = serde_json::from_str(r#"{ "allowed_origin": [] }"#);
        assert!(parsed.is_err(), "未知键必须报错");
    }

    #[test]
    fn should_validate_port_and_host() {
        let zero_port = ServerSection {
            port: 0,
            ..ServerSection::default()
        };
        assert!(zero_port.validate().is_err());

        let bad_host = ServerSection {
            host: "not-an-ip".to_string(),
            ..ServerSection::default()
        };
        assert!(bad_host.validate().is_err());
        assert!(ServerSection::default().validate().is_ok());
    }

    #[test]
    fn should_reject_body_limit_out_of_range() {
        let too_small = ServerSection {
            body_limit: Byte::from_u64(16),
            ..ServerSection::default()
        };
        assert!(too_small.validate().is_err(), "16 字节过小");

        let too_big = ServerSection {
            body_limit: Byte::from_u64(1024 * 1024 * 1024),
            ..ServerSection::default()
        };
        assert!(too_big.validate().is_err(), "1 GiB 过大");

        let zero_timeout = ServerSection {
            request_timeout: Duration::ZERO,
            ..ServerSection::default()
        };
        assert!(zero_timeout.validate().is_err(), "超时为 0 应被拒绝");
    }

    #[test]
    fn should_reject_root_timeout_exempt_path() {
        let root = ServerSection {
            timeout_exempt_paths: vec!["/".to_string()],
            ..ServerSection::default()
        };
        assert!(root.validate().is_err(), "\"/\" 会让全站豁免超时");

        let relative = ServerSection {
            timeout_exempt_paths: vec!["api/stream".to_string()],
            ..ServerSection::default()
        };
        assert!(relative.validate().is_err());

        let ok = ServerSection {
            timeout_exempt_paths: vec!["/api/stream".to_string(), "/ws".to_string()],
            ..ServerSection::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn should_reject_wildcard_cors_in_production() {
        let s = CorsSection {
            allowed_origins: vec!["*".to_string()],
            ..CorsSection::default()
        };
        assert!(
            s.validate_for_env(false).is_ok(),
            "非生产只允许（仍会记录）"
        );
        let error = s.validate_for_env(true).unwrap_err();
        assert!(error.contains("allowed_origins"), "实际：{error}");
    }

    #[test]
    fn should_reject_origin_with_path_or_trailing_slash() {
        // 浏览器 Origin 不带路径：`https://a.com/` 永远匹配不上
        for origin in [
            "https://app.example.com/",
            "https://app.example.com/callback",
            "https://app.example.com?x=1",
            "app.example.com",
        ] {
            let s = CorsSection {
                allowed_origins: vec![origin.to_string()],
                ..CorsSection::default()
            };
            assert!(s.validate().is_err(), "\"{origin}\" 应被拒绝");
        }
    }

    #[test]
    fn should_accept_valid_origins_and_methods() {
        let s = CorsSection {
            allowed_origins: vec![
                "https://app.example.com".to_string(),
                "http://localhost:3000".to_string(),
                "https://[::1]:8443".to_string(),
            ],
            allowed_methods: vec!["GET".to_string(), "POST".to_string(), "DELETE".to_string()],
            ..CorsSection::default()
        };
        assert!(s.validate_for_env(true).is_ok());

        let bad_method = CorsSection {
            allowed_methods: vec!["*".to_string()],
            ..CorsSection::default()
        };
        assert!(bad_method.validate().is_err());

        let trace = CorsSection {
            allowed_methods: vec!["TRACE".to_string()],
            ..CorsSection::default()
        };
        assert!(trace.validate().is_err());
    }
}
