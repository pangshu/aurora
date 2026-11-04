//! 迁移入口。
//!
//! aurora 提供迁移的**执行与状态查询**；迁移脚本由应用侧声明（见 [`migrations`]）。
//!
//! - 开发期：`sea-orm-cli migrate up`，或调用 [`up`] / [`down`] / [`status`]。
//! - 启动期：`[database].migrate_on_start = true` → [`migrate_on_start`]
//!   （启动状态机第 4 步，**绑定监听之前**）。
//!
//! [`migrations`]: crate::db::migrations

pub use sea_orm_migration::MigratorTrait;

use crate::boot::startup::StartupError;
use crate::db::pool::DbPool;
use crate::errors::AppResult;

/// 执行全部未应用的迁移（`up`）。
///
/// 迁移走内层连接（[`DbPool::inner`]）：`MigratorTrait` 只接受
/// `IntoSchemaManagerConnection`，该 trait 仅对 `&DatabaseConnection` /
/// `&DatabaseTransaction` 实现，没有 `ConnectionTrait` 的通用实现。
/// 副作用是迁移的 DDL 不进 SQL 日志——正好，它们没有绑定参数、且噪声很大。
///
/// # Errors
/// 迁移失败返回映射后的 [`crate::errors::AppError`]（`50002`）。
pub async fn up<M: MigratorTrait>(db: &DbPool) -> AppResult<()> {
    M::up(db.inner(), None).await?;
    Ok(())
}

/// 回滚最近 `steps` 个迁移。
///
/// # Errors
/// 迁移失败返回映射后的 [`crate::errors::AppError`]（`50002`）。
pub async fn down<M: MigratorTrait>(db: &DbPool, steps: u32) -> AppResult<()> {
    M::down(db.inner(), Some(steps)).await?;
    Ok(())
}

/// 打印迁移状态（已应用 / 待应用）。
///
/// # Errors
/// 查询失败返回映射后的 [`crate::errors::AppError`]（`50002`）。
pub async fn status<M: MigratorTrait>(db: &DbPool) -> AppResult<()> {
    M::status(db.inner()).await?;
    Ok(())
}

/// 按配置决定是否在启动时执行迁移（启动状态机第 4 步）。
///
/// 启用表前缀时额外检查迁移记录表是否带前缀：多个应用共用一个数据库而共用
/// `seaql_migrations` 时，后启动的应用会认为对方的迁移「已应用」而跳过建表。
///
/// # Errors
/// 迁移失败返回 [`StartupError::Migration`]。
pub async fn migrate_on_start<M: MigratorTrait>(
    db: &DbPool,
    enabled: bool,
) -> Result<(), StartupError> {
    if !enabled {
        return Ok(());
    }
    warn_if_migration_table_unprefixed::<M>();
    tracing::info!("running database migrations");
    up::<M>(db)
        .await
        .map_err(|e| StartupError::Migration(e.to_string()))
}

/// 迁移记录表与本进程表前缀不一致时告警（见 [`migrate_on_start`]）。
fn warn_if_migration_table_unprefixed<M: MigratorTrait>() {
    let Some(prefix) = crate::db::prefix::current() else {
        return;
    };
    if prefix.is_empty() {
        return;
    }
    let table = M::migration_table_name().to_string();
    if !table.starts_with(prefix) {
        tracing::warn!(
            migration_table = table,
            table_prefix = prefix,
            "迁移记录表没有带表前缀：多应用共用一个数据库时会互相覆盖迁移版本。\
             请在 Migrator 里覆写 migration_table_name() -> \
             aurora::db::prefix::migration_table()"
        );
    }
}
