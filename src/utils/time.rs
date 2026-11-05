//! 时间与时区工具。
//!
//! # 应用时区
//!
//! 进程有一个**全局唯一的应用时区**（`[server].timezone`），启动时由
//! [`init`] 登记一次，之后不再变化：
//!
//! - **未配置（空串）** → `None`，全部走 [`chrono::Local`]（系统时区，含 `TZ` 环境变量）；
//! - **配置了**（如 `Asia/Shanghai`）→ `Some(Tz)`，与系统时区解耦。
//!
//! 它决定了三件事，三者必须用同一个值，否则同一条数据会被两种语义解读：
//!
//! 1. 写库前的墙上时间（[`now`]）；
//! 2. 读库后还原绝对时刻（[`from_local_naive`]）；
//! 3. 日志时间戳与 MySQL 会话时区（[`format_rfc3339_at`] / [`session_offset`]）。
//!
//! 时区是**进程级、不可热更新**的（与 [`crate::db::prefix`] 同构）：运行期改配置
//! 文件不会改变已登记的值，变更必须重启。

use std::sync::OnceLock;

use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset,
    SecondsFormat, TimeZone, Utc,
};
use chrono_tz::Tz;

/// 进程时区。`None` = 未配置 = 跟随系统（[`chrono::Local`]）。
static TIMEZONE: OnceLock<Option<Tz>> = OnceLock::new();

/// 返回当前 UTC 时间。
#[must_use]
pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

/// 当前 Unix 时间戳（秒）。
#[must_use]
pub fn now_unix_secs() -> i64 {
    now_utc().timestamp()
}

/// 解析 IANA 时区名（如 `Asia/Shanghai`）。
///
/// # Errors
/// 名称非法时返回 [`chrono_tz::ParseError`]。
pub fn parse_timezone(name: &str) -> Result<Tz, chrono_tz::ParseError> {
    name.parse::<Tz>()
}

/// 登记进程时区（`[server].timezone`）。
///
/// 空串（忽略首尾空白）表示**跟随系统时区**。
///
/// 幂等：同值可重复调用（应用入口与 [`crate::app::AppState::build`] 都会调）。
///
/// # Errors
/// 时区名非法，或进程内已登记为另一个值时返回错误说明。
pub fn init(name: &str) -> Result<(), String> {
    let name = name.trim();
    let next = if name.is_empty() {
        None
    } else {
        Some(parse_timezone(name).map_err(|_| {
            format!("[server].timezone = {name:?} 不是合法的 IANA 时区（如 Asia/Shanghai）")
        })?)
    };
    // `OnceLock::set` 失败时返回的是**本次尝试写入的值**（不是已存值），
    // 所以「是否冲突」必须拿 `TIMEZONE.get()` 来判断，不能看 `set` 的 Err。
    if let Some(current) = TIMEZONE.get() {
        return if *current == next {
            Ok(())
        } else {
            Err(conflict_error())
        };
    }
    // 竞态：别的线程可能刚好抢先登记，以已存值为准再比一次
    if TIMEZONE.set(next).is_err() {
        return if app_timezone() == next {
            Ok(())
        } else {
            Err(conflict_error())
        };
    }
    Ok(())
}

/// 已登记为另一个值时的错误说明。
fn conflict_error() -> String {
    "[server].timezone 已在进程内初始化且本次取值不同：时区是进程级设置，变更需重启"
        .to_string()
}

/// 当前进程时区；`None` = 未配置 = 跟随系统。
#[must_use]
pub fn app_timezone() -> Option<Tz> {
    TIMEZONE.get().copied().flatten()
}

/// **应用时钟**：当前时刻在应用时区下的墙上时间。
///
/// 写库（`create_time` / `update_time` 等 naive 列）的唯一入口。
#[must_use]
pub fn now() -> NaiveDateTime {
    to_local_naive(Utc::now())
}

/// 绝对时刻 → 应用时区的墙上时间。
#[must_use]
pub fn to_local_naive(utc: DateTime<Utc>) -> NaiveDateTime {
    to_local_naive_at(utc, app_timezone())
}

/// 墙上时间 → 绝对时刻（读库的唯一出口）。
#[must_use]
pub fn from_local_naive(naive: NaiveDateTime) -> DateTime<Utc> {
    from_local_naive_at(naive, app_timezone())
}

/// 应用时区下「今天 00:00:00.000」（范围查询下界）。
#[must_use]
pub fn today_start() -> NaiveDateTime {
    now().date().and_time(NaiveTime::MIN)
}

/// MySQL 会话时区串，如 `+08:00` / `-04:00` / `+00:00`。
///
/// 用于把 `DEFAULT CURRENT_TIMESTAMP(3)` 对齐到应用时区。
#[must_use]
pub fn session_offset() -> String {
    offset_string_at(Utc::now(), app_timezone())
}

// ---- 纯函数：显式传时区，不读全局（单测可重复） ----

/// [`to_local_naive`] 的纯函数版本。
#[must_use]
pub fn to_local_naive_at(utc: DateTime<Utc>, tz: Option<Tz>) -> NaiveDateTime {
    match tz {
        Some(tz) => utc.with_timezone(&tz).naive_local(),
        None => utc.with_timezone(&Local).naive_local(),
    }
}

/// [`from_local_naive`] 的纯函数版本。
#[must_use]
pub fn from_local_naive_at(naive: NaiveDateTime, tz: Option<Tz>) -> DateTime<Utc> {
    match tz {
        Some(tz) => resolve(tz, naive),
        None => resolve(Local, naive),
    }
}

/// 墙上时间 → 绝对时刻。
///
/// 夏令时边界的两条规则（写死，避免调用方各自处理）：
///
/// - **秋季回拨**（同一墙上时间出现两次）：取较早的一次（DST 侧）。代价是那一小时
///   的数据再次读出会偏 1 小时——无 DST 的时区（如 `Asia/Shanghai`）不受影响；
/// - **春季前跳**（该墙上时间不存在）：按跳变前的偏移解释，落点等于往后推 1 小时，
///   与 MySQL 的直觉一致。
fn resolve<Z: TimeZone>(tz: Z, naive: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt.with_timezone(&Utc),
        LocalResult::Ambiguous(earlier, _later) => earlier.with_timezone(&Utc),
        LocalResult::None => {
            let before = naive - Duration::hours(2);
            let secs = tz
                .offset_from_local_datetime(&before)
                .earliest()
                .map_or(0, |offset| i64::from(offset.fix().local_minus_utc()));
            DateTime::<Utc>::from_naive_utc_and_offset(naive - Duration::seconds(secs), Utc)
        }
    }
}

/// [`session_offset`] 的纯函数版本。
#[must_use]
pub fn offset_string_at(utc: DateTime<Utc>, tz: Option<Tz>) -> String {
    let secs = match tz {
        Some(tz) => tz
            .offset_from_utc_datetime(&utc.naive_utc())
            .fix()
            .local_minus_utc(),
        None => utc.with_timezone(&Local).offset().fix().local_minus_utc(),
    };
    let sign = if secs < 0 { '-' } else { '+' };
    let abs = secs.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

/// RFC3339 文本（日志时间戳用）。
///
/// `use_z = true`：**仅当偏移为 0 时**输出 `Z`，否则输出 `±HH:MM`。所以未配置且系统
/// 为 UTC 时，输出与历史行为逐字节一致。
#[must_use]
pub fn format_rfc3339_at(utc: DateTime<Utc>, tz: Option<Tz>, precision: SecondsFormat) -> String {
    match tz {
        Some(tz) => utc.with_timezone(&tz).to_rfc3339_opts(precision, true),
        None => utc.with_timezone(&Local).to_rfc3339_opts(precision, true),
    }
}

/// 该时区是否观测夏令时（启动告警与会话时区说明用）。
#[must_use]
pub fn observes_dst(tz: Tz) -> bool {
    let year = Utc::now().year();
    let at = |month: u32, day: u32| {
        NaiveDate::from_ymd_opt(year, month, day).map_or_else(
            || DateTime::<Utc>::from_naive_utc_and_offset(NaiveDateTime::default(), Utc),
            |date| {
                DateTime::<Utc>::from_naive_utc_and_offset(date.and_time(NaiveTime::MIN), Utc)
            },
        )
    };
    let offset_of = |utc: DateTime<Utc>| {
        tz.offset_from_utc_datetime(&utc.naive_utc())
            .fix()
            .local_minus_utc()
    };
    // 1 月与 7 月的偏移不同 ⇔ 该时区观测 DST
    offset_of(at(1, 1)) != offset_of(at(7, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHANGHAI: Option<Tz> = Some(chrono_tz::Asia::Shanghai);

    fn at(secs: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(secs)
            .expect("测试时间必须合法")
            .with_timezone(&Utc)
    }

    #[test]
    fn should_parse_iana_timezone() {
        assert!(parse_timezone("Asia/Shanghai").is_ok());
        assert!(parse_timezone("Not/AZone").is_err());
    }

    #[test]
    fn should_convert_utc_to_local_wall_clock() {
        let utc = at("2026-11-05T06:07:08.123Z");
        assert_eq!(
            to_local_naive_at(utc, SHANGHAI),
            NaiveDate::from_ymd_opt(2026, 11, 5)
                .unwrap()
                .and_hms_milli_opt(14, 7, 8, 123)
                .unwrap()
        );
        // 未配置（跟随系统）时，UTC 输入在 UTC 机器上是一样的墙上时间
        assert_eq!(
            to_local_naive_at(utc, Some(chrono_tz::UTC)),
            NaiveDate::from_ymd_opt(2026, 11, 5)
                .unwrap()
                .and_hms_milli_opt(6, 7, 8, 123)
                .unwrap()
        );
    }

    #[test]
    fn should_round_trip_local_naive() {
        let utc = at("2026-11-05T06:07:08.123Z");
        for tz in [SHANGHAI, Some(chrono_tz::UTC), Some(chrono_tz::America::New_York)] {
            let naive = to_local_naive_at(utc, tz);
            assert_eq!(from_local_naive_at(naive, tz), utc, "往返必须无损：{tz:?}");
        }
    }

    #[test]
    fn should_pick_earlier_instant_on_fall_back() {
        // 美东 2026-11-01 01:30 出现两次：EDT(-04:00) 与 EST(-05:00)，取较早的一次
        let naive = NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();
        let utc = from_local_naive_at(naive, Some(chrono_tz::America::New_York));
        assert_eq!(utc, at("2026-11-01T05:30:00Z"));
    }

    #[test]
    fn should_shift_gap_forward_on_spring_forward() {
        // 美东 2026-03-08 02:30 不存在，按跳变前偏移(-05:00)解释 → 07:30Z（=03:30 EDT）
        let naive = NaiveDate::from_ymd_opt(2026, 3, 8)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        let utc = from_local_naive_at(naive, Some(chrono_tz::America::New_York));
        assert_eq!(utc, at("2026-03-08T07:30:00Z"));
    }

    #[test]
    fn should_format_session_offset() {
        let winter = at("2026-01-15T00:00:00Z");
        let summer = at("2026-07-15T00:00:00Z");
        assert_eq!(offset_string_at(winter, SHANGHAI), "+08:00");
        assert_eq!(offset_string_at(winter, Some(chrono_tz::UTC)), "+00:00");
        assert_eq!(
            offset_string_at(summer, Some(chrono_tz::America::New_York)),
            "-04:00"
        );
        assert_eq!(
            offset_string_at(winter, Some(chrono_tz::America::New_York)),
            "-05:00"
        );
    }

    #[test]
    fn should_render_rfc3339_with_offset_and_zulu() {
        let utc = at("2026-11-05T06:07:08.123Z");
        assert_eq!(
            format_rfc3339_at(utc, SHANGHAI, SecondsFormat::Millis),
            "2026-11-05T14:07:08.123+08:00"
        );
        assert_eq!(
            format_rfc3339_at(utc, Some(chrono_tz::UTC), SecondsFormat::Millis),
            "2026-11-05T06:07:08.123Z",
            "偏移为 0 时必须仍输出 Z，与历史行为逐字节一致"
        );
        assert_eq!(
            format_rfc3339_at(utc, SHANGHAI, SecondsFormat::Micros),
            "2026-11-05T14:07:08.123000+08:00"
        );
    }

    #[test]
    fn should_detect_dst_zones() {
        assert!(!observes_dst(chrono_tz::Asia::Shanghai), "中国无夏令时");
        assert!(!observes_dst(chrono_tz::UTC));
        assert!(observes_dst(chrono_tz::America::New_York));
    }

    #[test]
    fn should_reject_empty_after_trim_as_system() {
        // 只校验解析路径：`init` 动全局，放到 tests/timezone.rs 里跑
        assert!(parse_timezone("").is_err(), "空串不是合法 IANA 名，init 会特殊处理");
        assert!(parse_timezone("  Asia/Shanghai  ").is_err(), "必须由 init 负责 trim");
    }
}
