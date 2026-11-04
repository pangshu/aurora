//! 流式读取（全量导出 / 大批量处理）。
//!
//! 分页与游标分页都要求调用方**反复发起查询**，不适合导出全量数据：
//!
//! | 方式 | 读完全部 `n` 行的总代价 | 说明 |
//! |---|---|---|
//! | Offset 分页 | `≈ n² / (2 * size)` | 越翻越慢，深页会拖垮数据库 |
//! | 游标分页 | `O(n)` | 每页成本恒定，但要自己驱动循环 |
//! | **流式读取** | `O(n)` | 结果集交给数据库游标，逐条消费，内存与总行数无关 |
//!
//! ```ignore
//! use aurora::db::{StreamExt, stream_all};
//!
//! let mut rows = stream_all(&state.db, user::Entity::find()).await?;
//! while let Some(row) = rows.next().await {
//!     let model = row?;            // 单条错误可跳过，也可直接中断
//!     writer.write(model).await?;  // 写文件 / 投递 / 重建索引
//! }
//! ```

use std::pin::Pin;

use futures_util::Stream;
use sea_orm::{ConnectionTrait, EntityTrait, Select, StreamTrait};

use crate::errors::{AppError, AppResult};

/// 消费流所需的扩展 trait 再导出，业务侧无需自行引入 `futures-util`。
pub use futures_util::StreamExt;

/// 流式读取 `select` 的全部结果。
///
/// 逐条产出、不在内存里堆积结果集。**错误按条产出**（`Item = AppResult<Model>`），
/// 调用方可以跳过坏行继续，也可以整体中断——不会因为一行失败丢掉整批。
///
/// 连接池（[`DbPool`](crate::db::DbPool)）与事务（[`DbTransaction`]）都实现了
/// `StreamTrait`，两者都能用。但流会**一直占用连接直到消费完毕**，所以大批量导出
/// 请在连接池上执行，不要放进长事务。
///
/// # Errors
/// 建立游标失败（连接不可用 / SQL 错误）时返回映射后的 [`AppError`]（`50002`）。
/// 建流之后的逐行错误在流里产出，不在返回值里。
pub async fn stream<'a, C, E>(
    conn: &'a C,
    select: Select<E>,
) -> AppResult<Pin<Box<dyn Stream<Item = AppResult<E::Model>> + Send + 'a>>>
where
    C: ConnectionTrait + StreamTrait + Send,
    E: EntityTrait,
    E::Model: Send,
{
    let rows = select.stream(conn).await?;
    let stream: Pin<Box<dyn Stream<Item = AppResult<E::Model>> + Send + 'a>> =
        Box::pin(rows.map(|row| row.map_err(AppError::from)));
    Ok(stream)
}
