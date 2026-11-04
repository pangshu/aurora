//! 数据库连接池（SeaORM `DatabaseConnection` 的日志包装）。
//!
//! [`DbPool`] 不是 `DatabaseConnection` 的类型别名，而是**包装结构体**：它实现
//! [`ConnectionTrait`]、[`TransactionTrait`]、[`StreamTrait`] 并转发到内层，在
//! 转发前后采集 SQL 语句与绑定参数（见 [`sql_log`](crate::db::sql_log)）。
//! 所有查询最终都会落到 `ConnectionTrait` 的 `*_raw` 方法上，因此 ORM 查询、
//! [`Repository`](crate::db::Repository) 分页、流式读取、事务内语句都被覆盖。
//!
//! 需要原生能力（各驱动的 `get_*_connection_pool`、`set_metric_callback` 等）时走
//! [`DbPool::inner`]。**刻意不实现 `Deref`**：绕过包装层等于绕过日志，
//! 必须是显式且可 grep 的动作，而不是顺手一次解引用。

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    AccessMode, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction,
    DbBackend, DbErr, ExecResult, IsolationLevel, QueryResult, Statement, StreamTrait,
    TransactionError, TransactionOptions, TransactionSession, TransactionTrait,
};

use crate::boot::startup::StartupError;
use crate::config::sections::database::DatabaseSection;
use crate::db::sql_log::{LoggedSql as _, SqlLog};
use crate::errors::AppResult;
use crate::telemetry::HealthCheck;

/// SeaORM 连接池（带 SQL 参数日志）。
///
/// `Clone` 廉价：内层 `DatabaseConnection` 由 `Arc` 共享，日志配置本身也是 `Arc`。
/// 可直接用于 handler 的 `State<DbPool>`。
#[derive(Clone)]
pub struct DbPool {
    inner: DatabaseConnection,
    sql: Arc<SqlLog>,
}

/// 事务包装。
///
/// 与 [`DbPool`] 共享同一份 [`SqlLog`]（`Arc`），因此事务内的语句同样带参数日志。
/// **不实现 `Drop`**：内层 `DatabaseTransaction` 已有 `Drop` 负责残留回滚，
/// 再套一层会重复回滚。
pub struct DbTransaction {
    inner: DatabaseTransaction,
    sql: Arc<SqlLog>,
}

impl fmt::Debug for DbPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 不打印内层：`ConnectOptions` 可能带完整连接串（含口令）
        f.debug_struct("DbPool")
            .field("sql_log", &self.sql.enabled())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for DbTransaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DbTransaction")
            .field("sql_log", &self.sql.enabled())
            .finish_non_exhaustive()
    }
}

impl DbPool {
    /// 由已建好的连接构造（**不记录 SQL 日志**）。
    ///
    /// 用于测试、mock 与自建连接；生产路径请走 [`connect`]。
    #[must_use]
    pub fn from_connection(db: DatabaseConnection) -> Self {
        Self {
            inner: db,
            sql: Arc::new(SqlLog::disabled()),
        }
    }

    /// 由已建好的连接与日志配置构造。
    pub(crate) fn with_sql_log(db: DatabaseConnection, sql: SqlLog) -> Self {
        Self {
            inner: db,
            sql: Arc::new(sql),
        }
    }

    /// 内层连接（逃生口）。
    ///
    /// 迁移、各驱动的 pool getter、`set_metric_callback` 等原生能力从这里取。
    /// 注意：**经由内层执行的语句不会被 SQL 日志记录**。
    #[must_use]
    pub fn inner(&self) -> &DatabaseConnection {
        &self.inner
    }

    /// 取回内层连接（消费 `self`）。
    #[must_use]
    pub fn into_inner(self) -> DatabaseConnection {
        self.inner
    }

    /// 数据库后端类型。
    #[must_use]
    pub fn get_database_backend(&self) -> DbBackend {
        self.inner.get_database_backend()
    }

    /// 是否启用了 SQL 语句日志。
    #[must_use]
    pub fn sql_log_enabled(&self) -> bool {
        self.sql.enabled()
    }

    /// ping 数据库（连接可用性）。
    ///
    /// # Errors
    /// 连接不可用时返回底层 [`DbErr`]。
    pub async fn ping(&self) -> Result<(), DbErr> {
        self.inner.ping().await
    }

    /// 关闭连接池（消费 `self`）。
    ///
    /// # Errors
    /// 关闭过程中驱动报错时返回 [`DbErr`]。
    pub async fn close(self) -> Result<(), DbErr> {
        self.inner.close().await
    }

    /// 按引用关闭连接池。
    ///
    /// # Errors
    /// 关闭过程中驱动报错时返回 [`DbErr`]。
    pub async fn close_by_ref(&self) -> Result<(), DbErr> {
        self.inner.close_by_ref().await
    }
}

impl DbTransaction {
    /// 内层事务（逃生口）。
    #[must_use]
    pub fn inner(&self) -> &DatabaseTransaction {
        &self.inner
    }

    /// 取回内层事务（消费 `self`）。
    #[must_use]
    pub fn into_inner(self) -> DatabaseTransaction {
        self.inner
    }
}

fn wrap_transaction(inner: DatabaseTransaction, sql: &Arc<SqlLog>) -> DbTransaction {
    DbTransaction {
        inner,
        sql: Arc::clone(sql),
    }
}

/// 执行事务闭包并收尾（commit / rollback）。
///
/// `TransactionTrait::transaction` 在 trait 里**没有默认实现**，sea-orm 自己的
/// 实现用的是 crate 私有的 `Transaction::run`，外部拿不到，只能手写。
async fn run_transaction<F, T, E>(
    txn: DbTransaction,
    callback: F,
) -> Result<T, TransactionError<E>>
where
    F: for<'c> FnOnce(&'c DbTransaction) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
        + Send,
    T: Send,
    E: fmt::Display + fmt::Debug + Send,
{
    let result = callback(&txn).await.map_err(TransactionError::Transaction);
    match &result {
        Ok(_) => TransactionSession::commit(txn)
            .await
            .map_err(TransactionError::Connection)?,
        Err(_) => TransactionSession::rollback(txn)
            .await
            .map_err(TransactionError::Connection)?,
    }
    result
}

#[async_trait]
impl ConnectionTrait for DbPool {
    fn get_database_backend(&self) -> DbBackend {
        self.inner.get_database_backend()
    }

    async fn execute_raw(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        let event = self.sql.capture("execute", &stmt, false);
        let result = self.inner.execute_raw(stmt).await;
        match &result {
            Ok(done) => event.log_ok(Some(done.rows_affected())),
            Err(err) => event.log_err(err),
        }
        result
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        let event = self.sql.capture_sql("execute_unprepared", sql, false);
        let result = self.inner.execute_unprepared(sql).await;
        match &result {
            Ok(done) => event.log_ok(Some(done.rows_affected())),
            Err(err) => event.log_err(err),
        }
        result
    }

    async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        let event = self.sql.capture("query_one", &stmt, false);
        let result = self.inner.query_one_raw(stmt).await;
        match &result {
            Ok(row) => event.log_ok(Some(u64::from(row.is_some()))),
            Err(err) => event.log_err(err),
        }
        result
    }

    async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        let event = self.sql.capture("query_all", &stmt, false);
        let result = self.inner.query_all_raw(stmt).await;
        match &result {
            Ok(rows) => event.log_ok(Some(rows.len() as u64)),
            Err(err) => event.log_err(err),
        }
        result
    }
}

#[async_trait]
impl TransactionTrait for DbPool {
    type Transaction = DbTransaction;

    async fn begin(&self) -> Result<DbTransaction, DbErr> {
        let inner = self.inner.begin().await?;
        Ok(wrap_transaction(inner, &self.sql))
    }

    async fn begin_with_config(
        &self,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<DbTransaction, DbErr> {
        let inner = self
            .inner
            .begin_with_config(isolation_level, access_mode)
            .await?;
        Ok(wrap_transaction(inner, &self.sql))
    }

    async fn begin_with_options(
        &self,
        options: TransactionOptions,
    ) -> Result<DbTransaction, DbErr> {
        let inner = self.inner.begin_with_options(options).await?;
        Ok(wrap_transaction(inner, &self.sql))
    }

    async fn transaction<F, T, E>(&self, callback: F) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(&'c DbTransaction) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: fmt::Display + fmt::Debug + Send,
    {
        run_transaction(self.begin().await.map_err(TransactionError::Connection)?, callback).await
    }

    async fn transaction_with_config<F, T, E>(
        &self,
        callback: F,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(&'c DbTransaction) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: fmt::Display + fmt::Debug + Send,
    {
        let txn = self
            .begin_with_config(isolation_level, access_mode)
            .await
            .map_err(TransactionError::Connection)?;
        run_transaction(txn, callback).await
    }
}

#[async_trait]
impl ConnectionTrait for DbTransaction {
    fn get_database_backend(&self) -> DbBackend {
        // `DatabaseTransaction` 同时实现了 ConnectionTrait 与 StreamTrait，
        // 两者都有 `get_database_backend`，必须显式消歧
        ConnectionTrait::get_database_backend(&self.inner)
    }

    async fn execute_raw(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        let event = self.sql.capture("execute", &stmt, true);
        let result = self.inner.execute_raw(stmt).await;
        match &result {
            Ok(done) => event.log_ok(Some(done.rows_affected())),
            Err(err) => event.log_err(err),
        }
        result
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        let event = self.sql.capture_sql("execute_unprepared", sql, true);
        let result = self.inner.execute_unprepared(sql).await;
        match &result {
            Ok(done) => event.log_ok(Some(done.rows_affected())),
            Err(err) => event.log_err(err),
        }
        result
    }

    async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        let event = self.sql.capture("query_one", &stmt, true);
        let result = self.inner.query_one_raw(stmt).await;
        match &result {
            Ok(row) => event.log_ok(Some(u64::from(row.is_some()))),
            Err(err) => event.log_err(err),
        }
        result
    }

    async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        let event = self.sql.capture("query_all", &stmt, true);
        let result = self.inner.query_all_raw(stmt).await;
        match &result {
            Ok(rows) => event.log_ok(Some(rows.len() as u64)),
            Err(err) => event.log_err(err),
        }
        result
    }
}

#[async_trait]
impl TransactionTrait for DbTransaction {
    type Transaction = DbTransaction;

    async fn begin(&self) -> Result<DbTransaction, DbErr> {
        // 内层在已有事务上 begin 会降级为 SAVEPOINT
        let inner = self.inner.begin().await?;
        Ok(wrap_transaction(inner, &self.sql))
    }

    async fn begin_with_config(
        &self,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<DbTransaction, DbErr> {
        let inner = self
            .inner
            .begin_with_config(isolation_level, access_mode)
            .await?;
        Ok(wrap_transaction(inner, &self.sql))
    }

    async fn begin_with_options(
        &self,
        options: TransactionOptions,
    ) -> Result<DbTransaction, DbErr> {
        let inner = self.inner.begin_with_options(options).await?;
        Ok(wrap_transaction(inner, &self.sql))
    }

    async fn transaction<F, T, E>(&self, callback: F) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(&'c DbTransaction) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: fmt::Display + fmt::Debug + Send,
    {
        run_transaction(self.begin().await.map_err(TransactionError::Connection)?, callback).await
    }

    async fn transaction_with_config<F, T, E>(
        &self,
        callback: F,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(&'c DbTransaction) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: fmt::Display + fmt::Debug + Send,
    {
        let txn = self
            .begin_with_config(isolation_level, access_mode)
            .await
            .map_err(TransactionError::Connection)?;
        run_transaction(txn, callback).await
    }
}

#[async_trait]
impl TransactionSession for DbTransaction {
    async fn commit(self) -> Result<(), DbErr> {
        // inherent 优先：不会递归回本实现
        self.inner.commit().await
    }

    async fn rollback(self) -> Result<(), DbErr> {
        self.inner.rollback().await
    }
}

impl StreamTrait for DbPool {
    type Stream<'a>
        = <DatabaseConnection as StreamTrait>::Stream<'a>
    where
        Self: 'a;

    fn get_database_backend(&self) -> DbBackend {
        self.inner.get_database_backend()
    }

    fn stream_raw<'a>(
        &'a self,
        stmt: Statement,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Stream<'a>, DbErr>> + 'a + Send>> {
        let event = self.sql.capture("stream", &stmt, false);
        Box::pin(async move {
            let result = StreamTrait::stream_raw(&self.inner, stmt).await;
            match &result {
                Ok(_) => event.log_ok(None),
                Err(err) => event.log_err(err),
            }
            result
        })
    }
}

impl StreamTrait for DbTransaction {
    type Stream<'a>
        = <DatabaseTransaction as StreamTrait>::Stream<'a>
    where
        Self: 'a;

    fn get_database_backend(&self) -> DbBackend {
        StreamTrait::get_database_backend(&self.inner)
    }

    fn stream_raw<'a>(
        &'a self,
        stmt: Statement,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Stream<'a>, DbErr>> + 'a + Send>> {
        let event = self.sql.capture("stream", &stmt, true);
        Box::pin(async move {
            let result = StreamTrait::stream_raw(&self.inner, stmt).await;
            match &result {
                Ok(_) => event.log_ok(None),
                Err(err) => event.log_err(err),
            }
            result
        })
    }
}

/// 按配置连接数据库（启动状态机第 3 步）。
///
/// 建连前先注册 `[database].table_prefix`（进程级全局状态）：所有构造路径
/// （`AppState::build`、`TestApp`、应用自建连接）都经过这里，不存在"连上了
/// 但表名还没定"的窗口。
///
/// # Errors
/// 表前缀非法 / 与前一次初始化冲突，或建连失败时返回 [`StartupError`]。
pub async fn connect(cfg: &DatabaseSection) -> Result<DbPool, StartupError> {
    crate::db::prefix::init(&cfg.table_prefix).map_err(StartupError::Config)?;

    // `sql_log` 是 `sqlx_logging` 的超集（同样的语句 + 绑定参数）：
    // 两者同开会为同一条语句产生两条事件，而 sqlx 那条信息严格更少。
    // 这里让它让位，`DatabaseSection::validate` 已在启动时告警。
    let sqlx_logging = cfg.sqlx_logging && !cfg.sql_log.enabled;

    let mut options = ConnectOptions::new(cfg.url.clone());
    options
        .max_connections(cfg.max_connections)
        .min_connections(cfg.min_connections)
        .connect_timeout(cfg.connect_timeout)
        .acquire_timeout(cfg.acquire_timeout)
        .idle_timeout(cfg.idle_timeout)
        .max_lifetime(cfg.max_lifetime)
        // 必须显式传配置值：`ConnectOptions` 的默认是 `true`，漏传会把语句日志打开
        .sqlx_logging(sqlx_logging);

    let db = Database::connect(options)
        .await
        // 驱动原文可能带完整连接串（含口令）：进日志前先抹掉 userinfo
        .map_err(|e| {
            StartupError::Resource(format!(
                "database connect failed: {}",
                crate::utils::string::redact_url_secrets(&e.to_string())
            ))
        })?;

    tracing::info!(
        table_prefix = %cfg.table_prefix,
        sqlx_logging,
        sql_log = cfg.sql_log.enabled,
        "database connected"
    );
    Ok(DbPool::with_sql_log(
        db,
        SqlLog::new(cfg.sql_log.enabled, cfg.sql_log.slow_threshold),
    ))
}

/// ping 数据库（连接可用性）。
///
/// # Errors
/// 连接不可用时返回映射后的 [`crate::errors::AppError`]（`50002`）。
pub async fn ping(db: &DbPool) -> AppResult<()> {
    db.ping().await?;
    Ok(())
}

/// 数据库就绪探针（供 `/ready` 使用）。
///
/// `Err` 里给的是 `DbErr` 原文（可能含用户名、库名、表名与驱动信息）：注册表
/// （`telemetry::HealthRegistry::run`）会把它**只写进日志**，对外统一替换成
/// `database unavailable`——`/ready` 对未认证调用方可见。
#[derive(Debug, Clone)]
pub struct DbHealth {
    db: DbPool,
}

impl DbHealth {
    /// 由连接池构造探针。
    #[must_use]
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl HealthCheck for DbHealth {
    fn name(&self) -> &str {
        "database"
    }

    async fn check(&self) -> Result<(), String> {
        self.db.ping().await.map_err(|e| e.to_string())
    }
}
