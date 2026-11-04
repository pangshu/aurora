//! 数据库模块（feature `db`）。
//!
//! 基于 **SeaORM 2**（见 `docs/10`）：连接池、迁移入口、事务助手、游标分页、
//! 流式读取与 [`Repository`] 抽象。**业务实体由业务侧声明**；框架不内置 CRUD /
//! 软删 / 审计。
//!
//! [`DbPool`] / [`DbTransaction`] 是 SeaORM 连接的**包装层**：在转发前后采集
//! SQL 语句与绑定参数（`[database].sql_log`，target `aurora::sql`）。需要原生
//! 能力时用 `DbPool::inner()`，但经由内层执行的语句不会被记录。
//!
//! 驱动由 `db-postgres` / `db-sqlite` / `db-mysql` 三选一（互斥）。
//!
//! 多应用共用一个数据库时，用 `[database].table_prefix` 做表级隔离：
//! 实体改用 [`aurora_entity`] 声明、迁移用 [`prefix::ident`] / [`prefix::index`]，
//! 同一份代码即可落到 `book_user` / `study_user`（见 [`prefix`]）。

pub mod cursor;
mod error;
pub mod migration;
pub mod migrations;
pub mod pool;
pub mod prefix;
pub mod repository;
mod sql_log;
pub mod stream;
pub mod txn;

/// 表前缀感知的实体声明宏（替代 `DeriveEntityModel` + `#[sea_orm(table_name)]`）。
pub use aurora_macros::aurora_entity;
pub use cursor::{Cursor, paginate};
pub use migration::{MigratorTrait, down, migrate_on_start, status, up};
pub use pool::{DbHealth, DbPool, DbTransaction, connect, ping};
pub use repository::{Repository, require_found};
pub use stream::{StreamExt, stream as stream_all};
pub use txn::{is_serialization_failure, transaction, transaction_with_retry};

/// SeaORM 重导出（业务侧声明实体 / 查询时使用）。
pub use sea_orm;
