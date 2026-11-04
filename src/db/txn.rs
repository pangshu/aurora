//! 事务助手。
//!
//! - [`transaction`]：闭包式事务，`Ok` 自动 commit、`Err` 自动 rollback。
//! - [`transaction_with_retry`]：在事务助手内封装**序列化失败重试**，
//!   避免业务重复实现（见 `docs/10` §5）。
//! - [`is_serialization_failure`]：并发冲突判定（Postgres `40001` / 死锁 /
//!   SQLite 锁冲突）。

use std::future::Future;
use std::pin::Pin;

use sea_orm::{DbErr, TransactionError, TransactionTrait};

use crate::db::pool::{DbPool, DbTransaction};
use crate::errors::{AppError, AppResult};

/// 事务闭包类型：接收 `&DbTransaction`，返回 boxed future。
pub type TxFuture<'c, T> = Pin<Box<dyn Future<Output = AppResult<T>> + Send + 'c>>;

/// 在事务中执行 `f`：返回 `Ok` 自动 commit，返回 `Err` 自动 rollback。
///
/// ```no_run
/// # async fn run(db: aurora::db::DbPool) -> aurora::errors::AppResult<()> {
/// use aurora::db::transaction;
///
/// transaction(&db, |txn| {
///     Box::pin(async move {
///         // 在 txn 上执行写操作……
///         Ok(())
///     })
/// })
/// .await?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
/// 事务执行失败或闭包返回 `Err` 时返回对应 [`AppError`]。
pub async fn transaction<F, T>(db: &DbPool, f: F) -> AppResult<T>
where
    F: for<'c> FnOnce(&'c DbTransaction) -> TxFuture<'c, T> + Send,
    T: Send,
{
    db.transaction(f).await.map_err(map_transaction_error)
}

/// 带重试的事务：遇到并发冲突（序列化失败 / 死锁 / SQLite 锁）时最多重试
/// `max_attempts` 次（含首次）。
///
/// 闭包需为 `Fn`（可重复调用），因此内部不应持有跨次重试的状态。
///
/// # Errors
/// 重试耗尽或遇到不可重试错误时返回对应 [`AppError`]。
pub async fn transaction_with_retry<F, T>(db: &DbPool, max_attempts: u32, f: F) -> AppResult<T>
where
    F: for<'c> Fn(&'c DbTransaction) -> TxFuture<'c, T> + Send + Sync,
    T: Send,
{
    let attempts = max_attempts.max(1);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match db.transaction(&f).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                let retryable = transaction_db_err(&err).is_some_and(is_serialization_failure);
                if retryable && attempt < attempts {
                    tracing::warn!(attempt, "retrying transaction after serialization failure");
                    continue;
                }
                return Err(map_transaction_error(err));
            }
        }
    }
}

/// 判断数据库错误是否为并发冲突（可安全重试）。
///
/// 采用**文本匹配**做尽力而为的判定：覆盖 Postgres `40001` / `40P01`（死锁）、
/// 常见错误文案与 SQLite 的锁冲突。
#[must_use]
pub fn is_serialization_failure(err: &DbErr) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    const NEEDLES: [&str; 6] = [
        "40001",
        "40p01",
        "could not serialize access",
        "deadlock detected",
        "database is locked",
        "database table is locked",
    ];
    NEEDLES.iter().any(|needle| text.contains(needle))
}

/// 由 `TransactionError<AppError>` 取出底层 `DbErr`（若有）。
fn transaction_db_err(err: &TransactionError<AppError>) -> Option<&DbErr> {
    match err {
        TransactionError::Connection(db_err) => Some(db_err),
        TransactionError::Transaction(_) => None,
    }
}

/// 将 `TransactionError<AppError>` 映射为 [`AppError`]。
fn map_transaction_error(err: TransactionError<AppError>) -> AppError {
    match err {
        TransactionError::Connection(db_err) => AppError::from(db_err),
        TransactionError::Transaction(app_err) => app_err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_detect_serialization_failure_text() {
        assert!(is_serialization_failure(&DbErr::Query(
            sea_orm::RuntimeErr::Internal(
                "ERROR: could not serialize access due to concurrent update (SQLSTATE 40001)"
                    .into()
            )
        )));
        assert!(!is_serialization_failure(&DbErr::Query(
            sea_orm::RuntimeErr::Internal("syntax error".into())
        )));
    }
}
