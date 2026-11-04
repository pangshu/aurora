//! 数据访问抽象。
//!
//! 框架**不内置 CRUD / 软删 / 审计**（见 `docs/10` §8）；写操作与领域查询由业务侧
//! 定义自己的 Repository trait 实现（见 `docs/10` §4）。本模块提供：
//!
//! - [`Repository`]：为任意 SeaORM 实体提供**分页读取**（Offset / 游标），
//!   直接实现于 [`DbPool`] 与 [`DbTransaction`]，也可被替身替换（便于测试）。
//! - [`require_found`]：把「未找到」映射为 `30001`。

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait};

use crate::db::pool::{DbPool, DbTransaction};
use crate::errors::{AppError, AppResult};
use crate::utils::pagination::{Page, clamp_page_size};

/// 数据访问抽象：为 SeaORM 实体提供分页读取。
///
/// 默认实现覆盖两类读取；业务侧可自行实现更贴近领域的 trait（见 `docs/10` §4），
/// 或在测试中用替身替换本 trait。
#[async_trait]
pub trait Repository<E>
where
    E: EntityTrait,
    E::Model: Send + Sync + 'static,
{
    /// 底层连接（连接池或事务）。
    type Connection: ConnectionTrait;

    /// 返回底层连接。
    fn connection(&self) -> &Self::Connection;

    /// Offset 分页查询。
    ///
    /// `page` 从 1 开始；`size` 会被收敛到 `[1, MAX_PAGE_SIZE]`。
    ///
    /// ⚠️ 深分页会退化为 `OFFSET` 扫描：数据库必须取回并丢弃前 `(page - 1) * size`
    /// 行，翻得越深越慢，翻完 `n` 行的总代价约 `n² / (2 * size)`。长列表与全量导出
    /// 请改用 [`crate::db::cursor::paginate`]（keyset 分页，每页成本恒定）。
    ///
    /// # Errors
    /// 查询失败返回映射后的 [`AppError`]（`50002`）。
    async fn page(&self, page: u64, size: u64) -> AppResult<Page<E::Model>> {
        let size = clamp_page_size(size);
        let page = page.max(1);
        let paginator = E::find().paginate(self.connection(), size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;
        Ok(Page::new(items, page, size, total))
    }

    /// 实体总数。
    ///
    /// # Errors
    /// 查询失败返回映射后的 [`AppError`]（`50002`）。
    async fn count(&self) -> AppResult<u64> {
        let count = E::find().count(self.connection()).await?;
        Ok(count)
    }
}

#[async_trait]
impl<E> Repository<E> for DbPool
where
    E: EntityTrait,
    E::Model: Send + Sync + 'static,
{
    type Connection = DbPool;

    fn connection(&self) -> &Self::Connection {
        self
    }
}

#[async_trait]
impl<E> Repository<E> for DbTransaction
where
    E: EntityTrait,
    E::Model: Send + Sync + 'static,
{
    type Connection = DbTransaction;

    fn connection(&self) -> &Self::Connection {
        self
    }
}

/// 把「未找到」的 [`Option`] 映射为 `30001`。
///
/// # Errors
/// `None` 时返回 `AppError::not_found`。
pub fn require_found<T>(value: Option<T>, message: impl Into<String>) -> AppResult<T> {
    value.ok_or_else(|| AppError::not_found(message))
}
