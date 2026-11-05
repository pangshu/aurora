//! pretty 格式的时间戳。
//!
//! stock `fmt::layer()` 的默认 timer 是 `tracing_subscriber::fmt::time::SystemTime`，
//! 它把 Unix 秒数直接做 civil 换算并**硬写 `Z` 后缀**——输出永远是 UTC，与
//! `[server].timezone` 无关（见 `tracing-subscriber/src/fmt/time/datetime.rs`）。
//!
//! 所以这里替换成自己的 [`FormatTime`]：与 JSON 分支共用
//! [`crate::utils::time::format_rfc3339_at`]，只是精度保持历史上的 **6 位微秒**
//! （JSON 是 3 位毫秒），避免动运维侧的日志正则。
//!
//! `with_timer` 只存在于 stock `Format` 的 layer 上：JSON 分支已经 `event_format(...)`，
//! 拿不到这个方法，所以两条路径必须分别处理。

use std::fmt;

use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

use crate::utils::time::{app_timezone, format_rfc3339_at};

/// 应用时区、微秒精度的 timer。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LocalTime;

impl LocalTime {
    /// 纯函数版本（不读全局，便于单测）。
    pub(crate) fn render(now: DateTime<Utc>, tz: Option<Tz>) -> String {
        format_rfc3339_at(now, tz, SecondsFormat::Micros)
    }
}

impl FormatTime for LocalTime {
    fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
        // `FormatTime` 契约要求自己补分隔用的空格（stock 实现同样带尾随空格）
        write!(w, "{} ", Self::render(Utc::now(), app_timezone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(secs)
            .expect("测试时间必须合法")
            .with_timezone(&Utc)
    }

    #[test]
    fn should_render_micros_with_offset() {
        let utc = at("2026-11-05T06:07:08.123456Z");
        assert_eq!(
            LocalTime::render(utc, Some(chrono_tz::Asia::Shanghai)),
            "2026-11-05T14:07:08.123456+08:00"
        );
        assert_eq!(
            LocalTime::render(utc, Some(chrono_tz::UTC)),
            "2026-11-05T06:07:08.123456Z",
            "UTC 仍写 Z，与 stock timer 的历史输出一致"
        );
    }
}
