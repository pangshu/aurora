//! SQL 语句日志（带绑定参数）。
//!
//! sqlx 的 `QueryLogger` 只拿得到 SQL 文本——参数在它内部已经编码成二进制
//! `Arguments`，**原理上无法还原**。所以在 `Statement` 还在手上的 SeaORM 层采集：
//! [`ConnectionTrait`](sea_orm::ConnectionTrait) 的 `*_raw` 方法是所有查询的唯一
//! 必经入口，`DbPool` / `DbTransaction` 在那里记录耗时与结果。
//!
//! 事件的两个核心字段：
//! - `sql`：**占位符原文**（Postgres 为 `$1..$n`，MySQL / SQLite 为 `?`）。保留原文
//!   而不是内联后的完整语句，是为了让同一条语句的不同参数共享同一个「指纹」，
//!   慢查询聚合与 `pg_stat_statements` 之类的外部视图才能对上；
//! - `params`：按绑定顺序渲染的**SQL 字面量列表**（`['alice', 30, NULL]`），
//!   由 [`sea_query::Value`] 的 `Display` 生成——不用自己维护几十个类型分支，
//!   代价是转义走通用 writer（反斜杠），不随目标 backend 变化，见 `render_value`。
//!
//! 参数**原样输出、不做脱敏**（开关是唯一防线，见 `[database].sql_log`），
//! 但仍做两件事：控制字符替换为空格（防日志注入）、单值超长截断（防单条事件
//! 被一个 10 MB 的 `TEXT` 参数撑爆）。

use std::time::{Duration, Instant};

use sea_orm::sea_query::{Value, Values};
use sea_orm::{DbErr, Statement};

use crate::utils::string::log_safe;

/// 单个参数值渲染后的最大字符数（超出截断，末尾补 `…`）。
const MAX_PARAM_CHARS: usize = 1024;

/// 一条 SQL 语句文本的最大字符数。
///
/// 批量插入的语句本身就可能很长（上百个占位符），这里给的余量足够覆盖正常场景；
/// 保留上限只是为了让日志体积有一个可预期的上界。
const MAX_SQL_CHARS: usize = 8 * 1024;

/// 错误文本的最大字符数（与 `db::error` 的口径一致）。
const MAX_ERROR_CHARS: usize = 512;

/// SQL 日志运行时配置。
///
/// 随 `DbPool` 一起持有（`Arc` 共享给事务），**不用进程级全局状态**：
/// 全局开关会让「同一进程两个库 / 两种配置」和并发测试互相污染，
/// 与 `db::prefix` 踩过的坑同源。
#[derive(Debug, Clone)]
pub(crate) struct SqlLog {
    enabled: bool,
    /// `Duration::ZERO` = 不升级慢查询。
    slow_threshold: Duration,
}

impl SqlLog {
    /// 关闭状态的日志器（不产生任何事件）。
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            slow_threshold: Duration::ZERO,
        }
    }

    /// 由配置构造。
    pub(crate) fn new(enabled: bool, slow_threshold: Duration) -> Self {
        Self {
            enabled,
            slow_threshold,
        }
    }

    /// 是否已启用。
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// 采集一条带参数的语句。
    ///
    /// 关闭时**首行即返回**：不 clone SQL、不渲染参数、不取 `Instant`，
    /// 每条语句的代价是一次 `bool` 判断。
    pub(crate) fn capture(
        &self,
        op: &'static str,
        stmt: &Statement,
        in_transaction: bool,
    ) -> Option<SqlEvent> {
        if !self.enabled {
            return None;
        }
        Some(SqlEvent {
            op,
            sql: log_safe(&stmt.sql, MAX_SQL_CHARS),
            params: render_params(stmt.values.as_ref()),
            in_transaction,
            slow_threshold: self.slow_threshold,
            started: Instant::now(),
        })
    }

    /// 采集一条**无参数**的语句（`execute_unprepared`）。
    pub(crate) fn capture_sql(
        &self,
        op: &'static str,
        sql: &str,
        in_transaction: bool,
    ) -> Option<SqlEvent> {
        if !self.enabled {
            return None;
        }
        Some(SqlEvent {
            op,
            sql: log_safe(sql, MAX_SQL_CHARS),
            params: "[]".to_string(),
            in_transaction,
            slow_threshold: self.slow_threshold,
            started: Instant::now(),
        })
    }
}

/// 成功事件（两个分支只差级别，字段一致）。
///
/// `macro_rules!` 是文本作用域，必须定义在使用之前；级别作为 `ident` 拼进宏路径，
/// 从而保持每次调用都是编译期常量。
macro_rules! emit_ok {
    ($level:ident, $event:ident, $elapsed_ms:expr, $slow:expr, $rows:expr) => {
        match $rows {
            Some(rows) => tracing::$level!(
                target: "aurora::sql",
                op = $event.op,
                sql = %$event.sql,
                params = %$event.params,
                in_transaction = $event.in_transaction,
                elapsed_ms = $elapsed_ms,
                slow = $slow,
                rows,
                "sql"
            ),
            None => tracing::$level!(
                target: "aurora::sql",
                op = $event.op,
                sql = %$event.sql,
                params = %$event.params,
                in_transaction = $event.in_transaction,
                elapsed_ms = $elapsed_ms,
                slow = $slow,
                "sql"
            ),
        }
    };
}

/// 失败事件。
macro_rules! emit_err {
    ($level:ident, $event:ident, $elapsed_ms:expr, $error:expr) => {
        tracing::$level!(
            target: "aurora::sql",
            op = $event.op,
            sql = %$event.sql,
            params = %$event.params,
            in_transaction = $event.in_transaction,
            elapsed_ms = $elapsed_ms,
            error = %$error,
            "sql failed"
        );
    };
}

/// 已采集、待上报的一条语句。
pub(crate) struct SqlEvent {
    op: &'static str,
    sql: String,
    params: String,
    in_transaction: bool,
    slow_threshold: Duration,
    started: Instant,
}

impl SqlEvent {
    fn elapsed_ms(&self) -> u64 {
        // `as_millis()` 是 u128；长事务里的极端耗时也不会溢出 u64
        self.started.elapsed().as_millis() as u64
    }

    fn is_slow(&self, elapsed_ms: u64) -> bool {
        self.slow_threshold > Duration::ZERO
            && elapsed_ms >= self.slow_threshold.as_millis() as u64
    }

    /// 成功路径：默认 `INFO`，达到慢查询阈值升 `WARN`。
    ///
    /// 级别必须是**编译期常量**（`tracing` 把它放进 `static CALLSITE`），
    /// 所以这里不能把 `Level` 当变量传，只能按分支各调一次宏。
    pub(crate) fn log_ok(self, rows: Option<u64>) {
        let elapsed_ms = self.elapsed_ms();
        if self.is_slow(elapsed_ms) {
            emit_ok!(warn, self, elapsed_ms, true, rows);
        } else {
            emit_ok!(info, self, elapsed_ms, false, rows);
        }
    }

    /// 失败路径：`ERROR` + `error` 字段。
    ///
    /// `RecordNotFound` 是「没查到」而不是「执行失败」（业务常态，`one()` 分页越界
    /// 之类都会走到），固定 `ERROR` 会污染错误率告警，降为 `DEBUG`。
    pub(crate) fn log_err(self, err: &DbErr) {
        let elapsed_ms = self.elapsed_ms();
        let error = log_safe(&err.to_string(), MAX_ERROR_CHARS);
        if matches!(err, DbErr::RecordNotFound(_)) {
            emit_err!(debug, self, elapsed_ms, error);
        } else {
            emit_err!(error, self, elapsed_ms, error);
        }
    }
}

/// `capture` 返回的是 `Option<SqlEvent>`，调用点统一用这两个方法收尾，
/// 以免每处都写一遍 `if let Some(event) = ...`。
pub(crate) trait LoggedSql {
    /// 记一条成功事件（`rows` 语义见各调用点）。
    fn log_ok(self, rows: Option<u64>);
    /// 记一条失败事件。
    fn log_err(self, err: &DbErr);
}

impl LoggedSql for Option<SqlEvent> {
    fn log_ok(self, rows: Option<u64>) {
        if let Some(event) = self {
            event.log_ok(rows);
        }
    }

    fn log_err(self, err: &DbErr) {
        if let Some(event) = self {
            event.log_err(err);
        }
    }
}

/// 渲染参数列表：`['alice', 30, NULL]`。
fn render_params(values: Option<&Values>) -> String {
    let Some(values) = values else {
        return "[]".to_string();
    };
    if values.0.is_empty() {
        return "[]".to_string();
    }

    let mut out = String::with_capacity(8 + values.0.len() * 8);
    out.push('[');
    for (index, value) in values.0.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&render_value(value));
    }
    out.push(']');
    out
}

/// 单个参数的渲染。
///
/// 走 [`Value`] 的 `Display`（`CommonSqlQueryBuilder::write_value`）：字符串自动
/// 加单引号、`None` 变体输出 `NULL`、数字与布尔原样，覆盖全部类型分支且随
/// sea-query 升级自动跟进。
///
/// 转义风格是 sea-query 通用实现的**反斜杠转义**（`'O\'Brien'`），不是 SQL 标准的
/// 单引号翻倍（`'O''Brien'`）。`params` 是给人看的诊断字段，不承诺可直接回放成
/// 目标数据库的 SQL；要可执行的完整语句用 `Statement::Display`（按 backend 渲染）。
///
/// 只特判 `Bytes`：它的 `Display` 是完整的十六进制字面量（`x'...'`），
/// 而二进制列可能是图片、密钥材料或大文件，既打不全也不该落日志——只报长度。
fn render_value(value: &Value) -> String {
    match value {
        Value::Bytes(Some(bytes)) => format!("<{} bytes>", bytes.len()),
        other => {
            let rendered = other.to_string();
            // 参数值来自不可信输入：换行会把一条日志伪造成多行，必须单行化
            log_safe(&rendered, MAX_PARAM_CHARS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::Value;

    fn render(values: Vec<Value>) -> String {
        render_params(Some(&Values(values)))
    }

    #[test]
    fn should_render_values_in_bind_order() {
        let out = render(vec![
            Value::from("alice"),
            Value::from(30),
            Value::Int(None),
        ]);
        assert_eq!(out, "['alice', 30, NULL]");
    }

    #[test]
    fn should_quote_and_escape_strings() {
        // sea-query 的通用 writer 用反斜杠转义单引号（不是 SQL 标准的 `''`）
        assert_eq!(render(vec![Value::from("O'Brien")]), "['O\\'Brien']");
    }

    #[test]
    fn should_neutralize_control_characters() {
        // 参数含换行 → 必须单行化，否则可伪造日志行
        let out = render(vec![Value::from("a\nb\tc")]);
        assert!(!out.contains('\n'), "参数值里的换行必须被替换：{out}");
        assert!(!out.contains('\t'), "参数值里的制表符必须被替换：{out}");
    }

    #[test]
    fn should_report_bytes_length_only() {
        let out = render(vec![Value::Bytes(Some(vec![0u8; 12]))]);
        assert_eq!(out, "[<12 bytes>]");
        // 不得把内容以任何形式带进日志
        assert!(!out.contains("0000000000000000"));
    }

    #[test]
    fn should_truncate_oversized_value() {
        let long = "x".repeat(MAX_PARAM_CHARS * 2);
        let out = render(vec![Value::from(long)]);
        assert!(
            out.chars().count() <= MAX_PARAM_CHARS + 4,
            "超长参数必须被截断，实际长度 {}",
            out.chars().count()
        );
        assert!(out.ends_with("…]"), "截断后应保留收尾括号：{out}");
    }

    #[test]
    fn should_render_empty_params() {
        assert_eq!(render_params(None), "[]");
        assert_eq!(render_params(Some(&Values(Vec::new()))), "[]");
    }

    #[test]
    fn should_not_capture_when_disabled() {
        let log = SqlLog::disabled();
        let stmt = Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite,
            "SELECT ?",
            [Value::from(1)],
        );
        assert!(
            log.capture("query_all", &stmt, false).is_none(),
            "关闭时不得产生任何事件，也不应有渲染开销"
        );
        assert!(log.capture_sql("execute_unprepared", "SELECT 1", false).is_none());
    }

    #[test]
    fn should_capture_sql_and_params_when_enabled() {
        let log = SqlLog::new(true, Duration::ZERO);
        let stmt =
            Statement::from_sql_and_values(sea_orm::DbBackend::Sqlite, "SELECT ?", [Value::from(7)]);
        let event = log.capture("query_all", &stmt, true).expect("应采集到事件");
        assert_eq!(event.op, "query_all");
        assert_eq!(event.sql, "SELECT ?", "sql 字段必须是占位符原文");
        assert_eq!(event.params, "[7]");
        assert!(event.in_transaction);
        assert!(!event.is_slow(0), "阈值为 0 时不升级慢查询");
    }

    #[test]
    fn should_flag_slow_when_threshold_reached() {
        let log = SqlLog::new(true, Duration::from_millis(100));
        let stmt = Statement::from_string(sea_orm::DbBackend::Sqlite, "SELECT 1");
        let event = log.capture("execute", &stmt, false).expect("应采集到事件");
        assert!(!event.is_slow(99));
        assert!(event.is_slow(100));
    }
}
