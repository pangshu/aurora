//! `[database]` 配置段（feature `db`）。

use std::time::Duration;

use serde::Deserialize;

use crate::config::sections::de::de_duration;

/// 数据库配置段。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct DatabaseSection {
    /// 连接串（敏感项 🔒，只走环境变量 `AURORA_DATABASE__URL`；启用 `db` 时必填）。
    pub url: String,
    /// 表前缀（多应用共用一个数据库时的表级隔离，默认 `""` = 不启用）。
    ///
    /// 配置成 `"book"` 时，逻辑表 `user` 落到物理表 `book_user`；同一份业务代码
    /// 换个配置即落到 `study_user`。合法字符见
    /// [`prefix::validate`](crate::db::prefix::validate)：小写字母开头，仅含小写
    /// 字母 / 数字 / 下划线（表名会直接进 SQL，无法参数化，只能白名单）。
    ///
    /// 前缀是**进程级**状态且不可热更新：变更等价于数据迁移，必须重启。
    pub table_prefix: String,
    /// 最大连接数。
    pub max_connections: u32,
    /// 最小连接数。
    pub min_connections: u32,
    /// 建连超时。
    #[serde(deserialize_with = "de_duration")]
    pub connect_timeout: Duration,
    /// 从池获取连接的超时；耗尽 → `50005`（503）。
    #[serde(deserialize_with = "de_duration")]
    pub acquire_timeout: Duration,
    /// 空闲回收时长。
    #[serde(deserialize_with = "de_duration")]
    pub idle_timeout: Duration,
    /// 连接最大存活时长。
    #[serde(deserialize_with = "de_duration")]
    pub max_lifetime: Duration,
    /// 启动时执行迁移（启动状态机第 4 步，**绑定监听之前**）。
    pub migrate_on_start: bool,
    /// 允许明文连接数据库（**仅限本机 / 可信网络**）。
    ///
    /// 默认 `false`：`postgres` / `mysql` 连接串必须带**可验证证书**的 TLS 参数
    /// （`sslmode=verify-full|verify-ca`、`ssl-mode=VERIFY_CA|VERIFY_IDENTITY`），
    /// 否则启动失败。设为 `true` 会跳过该校验并在启动日志告警。
    ///
    /// 只加密不验签的 `sslmode=require` / `ssl-mode=REQUIRED` 同样会被拒绝：
    /// 加密挡不住中间人，必须验证书。
    pub allow_insecure: bool,
    /// 是否输出 sqlx 语句日志（每条 SQL 一条 tracing 事件，目标 `sqlx::query`）。
    ///
    /// 默认 `false`：打开等于把每条 SQL 灌进日志，只在开发 / 排障时开。
    /// 事件落在当前 span 内，因此请求期 SQL 会自带 `trace_id` / `method` / `path`。
    /// 是**建连时**生效的参数，改动需重启（不参与热更新）。
    ///
    /// 级别固定 `Info`（sea-orm 默认）：sqlx 对一条语句只发一个事件，info / debug /
    /// trace 的信息量一样，多暴露一级只会造成「配了 debug 却没日志」的困惑。
    pub sqlx_logging: bool,
}

impl Default for DatabaseSection {
    fn default() -> Self {
        Self {
            url: String::new(),
            table_prefix: String::new(),
            max_connections: 10,
            min_connections: 1,
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(600),
            max_lifetime: Duration::from_secs(1800),
            migrate_on_start: false,
            allow_insecure: false,
            sqlx_logging: false,
        }
    }
}

impl DatabaseSection {
    /// 校验：启用 `db` 时 `url` 必填；网络型驱动（`postgres` / `mysql`）必须启用
    /// **可验证证书**的 TLS。
    ///
    /// 这条校验存在的原因：sqlx 的 Postgres 默认 `sslmode=prefer`——服务端不支持
    /// SSL 时会**静默降级成明文**继续连，日志里只有一句 `database connected`。
    /// 生产环境在共享网络里等于把凭据和查询结果裸奔。
    ///
    /// # Errors
    /// `url` 为空、或未启用可验证 TLS 且未显式 `allow_insecure` 时返回错误信息。
    /// **错误信息不回显连接串**（其中含密码）。
    pub fn validate(&self) -> Result<(), String> {
        if self.url.trim().is_empty() {
            return Err("database.url is required when the `db` feature is enabled \
                 (set AURORA_DATABASE__URL)"
                .to_string());
        }

        // 表前缀会直接拼进 SQL 的表名（无法参数化绑定），必须白名单校验。
        crate::db::prefix::validate(&self.table_prefix)?;

        let scheme = scheme_of(&self.url);
        if self.allow_insecure {
            if matches!(scheme.as_str(), "postgres" | "postgresql" | "mysql") {
                tracing::warn!(
                    scheme = %scheme,
                    "database TLS verification disabled by [database].allow_insecure = true"
                );
            }
            return Ok(());
        }

        require_tls(&scheme, &self.url)
    }
}

/// 取连接串的 scheme（小写）。密码里可能含 `?` / `&`，但 scheme 位于第一个
/// `://` 之前，不受影响。
fn scheme_of(url: &str) -> String {
    url.split_once("://")
        .map(|(scheme, _)| scheme.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 从连接串 query 中取参数值（小写）。只做**尽力而为**的解析：不碰 userinfo，
/// 避免把含 `?` 的密码误当成参数——解析不出来时按「未启用」处理（fail-closed）。
fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        name.eq_ignore_ascii_case(key)
            .then(|| value.to_ascii_lowercase())
    })
}

/// 网络型驱动的 TLS 要求。`sqlite` 等本地驱动直接放行。
fn require_tls(scheme: &str, url: &str) -> Result<(), String> {
    let (param, allowed): (&str, &[&str]) = match scheme {
        "postgres" | "postgresql" => ("sslmode", &["verify-full", "verify-ca"]),
        "mysql" => ("ssl-mode", &["verify-ca", "verify-identity"]),
        _ => return Ok(()),
    };

    let value = query_param(url, param)
        .unwrap_or_default()
        .replace('_', "-");
    if allowed.contains(&value.as_str()) {
        return Ok(());
    }

    Err(format!(
        "database.url must enable verifiable TLS: set `{param}` to one of {allowed:?} \
         (current: {}); for local or trusted networks set [database].allow_insecure = true",
        if value.is_empty() {
            "unset"
        } else {
            "not verifiable"
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_require_url() {
        assert!(DatabaseSection::default().validate().is_err());
        let section = DatabaseSection {
            url: "sqlite::memory:".to_string(),
            ..DatabaseSection::default()
        };
        assert!(section.validate().is_ok());
    }

    #[test]
    fn should_validate_table_prefix() {
        let mut section = with_url("sqlite::memory:");
        assert_eq!(section.table_prefix, "", "默认不启用前缀");
        section.table_prefix = "book".to_string();
        assert!(section.validate().is_ok());

        section.table_prefix = "Book".to_string();
        let error = section.validate().unwrap_err();
        assert!(
            error.contains("table_prefix"),
            "错误信息应指出配置项，实际：{error}"
        );
    }

    #[test]
    fn should_parse_durations_and_limits() {
        let json = r#"
            {
                "url": "sqlite::memory:",
                "max_connections": 32,
                "connect_timeout": "3s",
                "idle_timeout": "5m"
            }
        "#;
        let s: DatabaseSection = serde_json::from_str(json).unwrap();
        assert_eq!(s.max_connections, 32);
        assert_eq!(s.connect_timeout, Duration::from_secs(3));
        assert_eq!(s.idle_timeout, Duration::from_secs(300));
        assert_eq!(s.min_connections, 1);
    }

    fn with_url(url: &str) -> DatabaseSection {
        DatabaseSection {
            url: url.to_string(),
            ..DatabaseSection::default()
        }
    }

    #[test]
    fn should_require_verifiable_tls_for_network_drivers() {
        // 未设置 sslmode：sqlx 会静默降级成明文 → 拒绝
        assert!(with_url("postgres://u:p@db:5432/app").validate().is_err());
        // 只加密不验签：挡不住中间人 → 拒绝
        assert!(
            with_url("postgres://u:p@db:5432/app?sslmode=require")
                .validate()
                .is_err()
        );
        assert!(
            with_url("postgres://u:p@db:5432/app?sslmode=disable")
                .validate()
                .is_err()
        );
        // 可验证证书 → 通过
        assert!(
            with_url("postgres://u:p@db:5432/app?sslmode=verify-full")
                .validate()
                .is_ok()
        );
        assert!(
            with_url("postgres://u:p@db:5432/app?sslmode=verify-ca&application_name=x")
                .validate()
                .is_ok()
        );
        // MySQL：`ssl-mode` 大小写与下划线不敏感
        assert!(with_url("mysql://u:p@db:3306/app").validate().is_err());
        assert!(
            with_url("mysql://u:p@db:3306/app?ssl-mode=REQUIRED")
                .validate()
                .is_err()
        );
        assert!(
            with_url("mysql://u:p@db:3306/app?ssl-mode=VERIFY_IDENTITY")
                .validate()
                .is_ok()
        );
        // SQLite 无 TLS 概念 → 放行
        assert!(with_url("sqlite::memory:").validate().is_ok());
        assert!(with_url("sqlite://./data/app.db").validate().is_ok());
    }

    #[test]
    fn should_not_echo_connection_string_in_tls_error() {
        let section = with_url("postgres://user:p4ssw0rd@db:5432/app");
        let err = section.validate().unwrap_err();
        assert!(!err.contains("p4ssw0rd"), "错误信息不得回显密码：{err}");
        assert!(
            !err.contains("postgres://"),
            "错误信息不得回显连接串：{err}"
        );
        assert!(err.contains("sslmode"), "应给出修复指引：{err}");
    }

    #[test]
    fn should_allow_insecure_only_when_explicitly_opted_in() {
        let section = DatabaseSection {
            url: "postgres://u:p@db:5432/app".to_string(),
            allow_insecure: true,
            ..DatabaseSection::default()
        };
        assert!(section.validate().is_ok());
    }

    #[test]
    fn should_treat_malformed_query_as_insecure() {
        // 密码含 `?` 时 query 解析不可靠 → 必须 fail-closed，不能误判为已启用 TLS
        assert!(
            with_url("postgres://u:p?ss@db:5432/app")
                .validate()
                .is_err()
        );
    }
}
