//! 表前缀：多个应用共用一个数据库时的表级隔离（feature `db`）。
//!
//! 配置项为 `[database].table_prefix`（环境变量 `AURORA_DATABASE__TABLE_PREFIX`）：
//!
//! - `table_prefix = "book"` → 逻辑表 `user` 落到物理表 `book_user`
//! - `table_prefix = ""`（默认）→ 保持原表名，行为与未启用前缀完全一致
//!
//! # 为什么表名要在运行时解析
//!
//! SeaORM 的表名来自派生宏生成的编译期常量（`EntityName::table_name -> &'static str`）。
//! 本模块把「拼好的物理表名」`Box::leak` 成 `&'static str` 并缓存，于是同一份业务代码
//! 在不同应用的配置下访问不同的表。实体侧由 [`aurora_entity`](crate::db::aurora_entity)
//! 宏接到这里，业务代码无感。
//!
//! # 边界（写进契约，不做隐式兜底）
//!
//! - **进程级唯一**：一个进程一套前缀。同进程多租户请改用独立进程或独立库。
//! - **只覆盖结构化查询**：`EntityTrait` / [`Repository`](crate::db::Repository) /
//!   迁移 helper 构造的 SQL 自动带前缀；手写 `Statement` 必须用 [`table`] / [`ident`]
//!   自行拼接，框架无法也不应猜测字符串里的表名。
//! - **不可热更新**：变更前缀等价于数据迁移，必须重启进程。
//! - **未初始化即 panic**：静默返回不带前缀的表名会把数据写进错误的表，比崩溃更糟。

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use sea_orm::sea_query::DynIden;

/// 标识符长度上限：PostgreSQL 为 63 字节，MySQL 为 64。取保守值。
const MAX_IDENT_BYTES: usize = 63;

/// 前缀自身的长度上限（给表名留出空间）。
const MAX_PREFIX_LEN: usize = 32;

struct State {
    /// 规范化前缀；空串表示不启用。
    prefix: String,
    /// 逻辑表名 → 物理表名。命中后直接返回 `&'static str`，热路径只有一次读锁。
    cache: RwLock<HashMap<&'static str, &'static str>>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// 校验前缀是否合法（配置校验与 [`init`] 共用同一套规则）。
///
/// 规则：空串（不启用）或 ASCII 小写标识符——小写字母开头，仅含小写字母、数字、
/// 下划线，不以 `_` 结尾、不含连续 `__`，长度不超过 32。
///
/// 前缀会**直接进入 SQL 文本**（表名无法参数化绑定），所以必须是白名单而不是转义。
///
/// # Errors
/// 前缀不合法时返回错误信息（不含任何敏感内容）。
pub fn validate(prefix: &str) -> Result<(), String> {
    if prefix.is_empty() {
        return Ok(());
    }
    if prefix.len() > MAX_PREFIX_LEN {
        return Err(format!(
            "[database].table_prefix 过长：{prefix:?} 超过 {MAX_PREFIX_LEN} 字节"
        ));
    }
    if !prefix.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(format!(
            "[database].table_prefix 必须以小写字母开头：{prefix:?}"
        ));
    }
    if !prefix
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "[database].table_prefix 只允许小写字母、数字与下划线：{prefix:?}"
        ));
    }
    if prefix.ends_with('_') || prefix.contains("__") {
        return Err(format!(
            "[database].table_prefix 不得以 `_` 结尾或包含连续 `__`：{prefix:?}\
             （分隔符由框架补，配置值不需要带）"
        ));
    }
    Ok(())
}

/// 注册前缀（幂等：同值可重复调用，异值返回错误）。
///
/// 由 [`connect`](crate::db::connect) 在建立连接时调用，应用侧通常不需要直接调用。
///
/// # Errors
/// 前缀不合法，或已被初始化为另一个值时返回错误信息。
pub fn init(prefix: &str) -> Result<(), String> {
    validate(prefix)?;

    if let Some(state) = STATE.get() {
        return same_or_error(&state.prefix, prefix);
    }

    // 竞态：并发首次初始化时只有一个 `set` 成功，随后按同值/异值规则判定即可。
    let _ = STATE.set(State {
        prefix: prefix.to_string(),
        cache: RwLock::new(HashMap::new()),
    });
    let state = STATE
        .get()
        .expect("OnceLock 在 set 之后必须可读（并发初始化返回值已被忽略）");
    same_or_error(&state.prefix, prefix)
}

fn same_or_error(existing: &str, requested: &str) -> Result<(), String> {
    if existing == requested {
        Ok(())
    } else {
        Err(format!(
            "[database].table_prefix 已初始化为 {existing:?}，不能再改为 {requested:?}：\
             前缀是进程级全局状态，一个进程只能对应一套表。\
             多个应用共用一个数据库时，请让它们各自跑在独立进程里"
        ))
    }
}

/// 当前生效的前缀；`None` 表示尚未初始化。
#[must_use]
pub fn current() -> Option<&'static str> {
    STATE.get().map(|state| state.prefix.as_str())
}

/// 是否启用了非空前缀。
#[must_use]
pub fn is_enabled() -> bool {
    current().is_some_and(|prefix| !prefix.is_empty())
}

/// 逻辑表名 → 物理表名（[`aurora_entity`](crate::db::aurora_entity) 宏使用）。
///
/// 返回值是 `&'static str`：首次调用时拼接并 `Box::leak` 一次，之后走缓存，
/// 因此每次 `EntityName::table_name` 的开销只是一次读锁 + 哈希查找。
///
/// # Panics
/// 尚未 [`init`] 时 panic。静默返回不带前缀的表名会把数据写到错误的表，
/// 所以这里选择 fail-fast（正常路径由 `AppState::build` → `db::connect` 保证先初始化）。
#[must_use]
pub fn table(base: &'static str) -> &'static str {
    let state = state_or_panic();
    if state.prefix.is_empty() {
        return base;
    }

    if let Some(name) = state
        .cache
        .read()
        .expect("表前缀缓存锁中毒")
        .get(base)
        .copied()
    {
        return name;
    }

    let mut cache = state.cache.write().expect("表前缀缓存锁中毒");
    // 双重检查：等写锁期间可能已有别的线程拼好。
    if let Some(name) = cache.get(base).copied() {
        return name;
    }
    let name: &'static str = Box::leak(format!("{}_{base}", state.prefix).into_boxed_str());
    warn_if_too_long(name);
    cache.insert(base, name);
    name
}

/// 逻辑标识符（表 / 序列 / schema 等）→ 物理标识符。
///
/// 用于迁移脚本与手写查询构造：`Table::create().table(...)` /
/// `Table::drop().table(...)` 这类接受 `IntoIden` 的位置直接可用；
/// 索引 / 约束 / 外键名请用 [`index`]（那里的签名要求 `Into<String>`）。
///
/// # Panics
/// 尚未 [`init`] 时 panic（理由同 [`table`]）。
#[must_use]
pub fn ident(base: &str) -> DynIden {
    DynIden::from(physical(base))
}

/// 索引 / 约束 / 外键名 → 物理名（**返回 `String`**）。
///
/// 与 [`ident`] 的区别只是类型：sea-query 的 `Index::name` / `ForeignKey::name`
/// 要求 `Into<String>` 而不是 `IntoIden`，所以这里直接给 `String`。
///
/// 索引名在 PostgreSQL 里是 schema 级唯一，多个应用共库时**必须**带前缀。
///
/// # Panics
/// 尚未 [`init`] 时 panic（理由同 [`table`]）。
#[must_use]
pub fn index(base: &str) -> String {
    physical(base)
}

fn physical(base: &str) -> String {
    let state = state_or_panic();
    if state.prefix.is_empty() {
        return base.to_string();
    }
    let name = format!("{}_{base}", state.prefix);
    warn_if_too_long(&name);
    name
}

/// 迁移记录表名（`seaql_migrations` 的带前缀版本）。
///
/// 多个应用共用一个数据库时，每个应用必须有**自己的**迁移记录表，否则会互相
/// 认为对方的迁移「已应用」而跳过建表。请在 `MigratorTrait` 里覆写：
///
/// ```ignore
/// fn migration_table_name() -> DynIden {
///     aurora::db::prefix::migration_table()
/// }
/// ```
#[must_use]
pub fn migration_table() -> DynIden {
    ident("seaql_migrations")
}

fn state_or_panic() -> &'static State {
    STATE.get().unwrap_or_else(|| {
        panic!(
            "表前缀未初始化：请经 AppState::build（内部会调用 db::connect + prefix::init）启动应用；\
             若手工建连，请先调用 aurora::db::prefix::init(&config.database.table_prefix)"
        )
    })
}

fn warn_if_too_long(name: &str) {
    if name.len() > MAX_IDENT_BYTES {
        tracing::warn!(
            identifier = name,
            len = name.len(),
            "带前缀的标识符超过 63 字节：PostgreSQL 会静默截断，可能造成同名表/索引冲突"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只测纯函数：`init` / `table` 会改动进程级全局状态，放进
    /// `tests/table_prefix.rs`（独立测试二进制）里验证。
    #[test]
    fn should_accept_legal_prefix() {
        for ok in ["", "book", "study", "app1_book"] {
            assert!(validate(ok).is_ok(), "应接受：{ok:?}");
        }
    }

    #[test]
    fn should_reject_illegal_prefix() {
        for bad in [
            "Book",
            "1book",
            "book-",
            "book;drop",
            "book__x",
            "book_",
            "书",
            " book",
        ] {
            assert!(validate(bad).is_err(), "应拒绝：{bad:?}");
        }
        assert!(
            validate(&"a".repeat(MAX_PREFIX_LEN + 1)).is_err(),
            "超长前缀应被拒绝"
        );
    }
}
