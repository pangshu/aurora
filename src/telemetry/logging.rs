//! 日志初始化。
//!
//! 基于 `tracing-subscriber` 构建全局 subscriber，支持 `RUST_LOG` 风格的
//! 级别过滤（默认取 `[telemetry].level`）与 JSON / pretty 两种格式。
//!
//! 输出去向由配置决定：`[telemetry].stdout`（默认开）与 `[telemetry].file.enabled`
//! （feature `log-file`，基于 rotate-rs 滚动文件）**可并存**；两者都没开会被
//! `TelemetrySection::validate` 在启动期拒掉。
//!
//! 过滤器包在 [`reload`] 层里，因此 `[telemetry].level` **可热更新**——
//! 由 [`set_level`] 替换过滤器，无需重启（见 `docs/08` §2.9）。
//!
//! 启用 `otel` feature 且 `[telemetry].otel.enabled = true` 时，subscriber 额外挂
//! OTel 导出层，从而同一份 span 既写日志也导出到 OTLP（见 [`crate::telemetry::trace`]）。

use std::io::{self, Write as _};
use std::sync::OnceLock;

use tracing::Dispatch;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::reload;

use crate::boot::startup::StartupError;
use crate::config::sections::TelemetrySection;
use crate::telemetry::format::LogFormat;

#[cfg(feature = "log-file")]
use crate::telemetry::appender::RotatingFile;

/// 全局过滤器句柄：`init` 成功后可用，供 [`set_level`] 热替换。
static FILTER: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

/// 日志输出目的地。
///
/// 把「控制台 / 文件 / 黑洞」收敛成同一个 `MakeWriter`，fmt 层因此只需两份
/// （json / pretty），而不是「格式 × 目的地 × otel」的组合爆炸。
#[derive(Clone)]
enum Target {
    Stdout,
    #[cfg(feature = "log-file")]
    File(RotatingFile),
    /// 未启用的占位：写入即丢弃（仍会走一遍格式化，成本可忽略）。
    Sink,
}

/// fmt 层的写入器工厂。
struct LogSink(Target);

/// 单条事件的行缓冲。
///
/// fmt 层把一条事件拆成多次 `write`（时间 / 级别 / target / 字段 / 换行），
/// 而 rotate-rs 的原子单位是**单次 write**：逐片直写会在多线程下交织成半行，
/// 轮转还可能在片段之间发生，把一条日志拆到两个文件。所以这里先攒进 `Vec`，
/// 事件结束时（Drop）一次性写出。
struct LineBuffer {
    target: Target,
    buf: Vec<u8>,
}

impl LineBuffer {
    fn emit(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let line = std::mem::take(&mut self.buf);
        match &self.target {
            Target::Stdout => {
                let mut out = io::stdout().lock();
                out.write_all(&line)?;
                out.flush()
            }
            #[cfg(feature = "log-file")]
            Target::File(file) => file.write_line(&line),
            Target::Sink => Ok(()),
        }
    }
}

impl io::Write for LineBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit()
    }
}

impl Drop for LineBuffer {
    fn drop(&mut self) {
        // fmt 层不保证调用 flush：事件结束时靠 Drop 落盘。写入错误在这里无处上报
        // （tracing 本就不回传），文件侧的持续失败由 rotate-rs 的 stderr 节流兜底。
        let _ = self.emit();
    }
}

impl<'a> fmt::MakeWriter<'a> for LogSink {
    type Writer = LineBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        LineBuffer {
            target: self.0.clone(),
            buf: Vec::with_capacity(512),
        }
    }
}

/// 初始化全局日志订阅器。
///
/// # Errors
/// 级别表达式非法、OTel 导出器初始化失败或全局 subscriber 已被初始化时返回
/// [`StartupError::Telemetry`]。
pub(crate) fn init(cfg: &TelemetrySection) -> Result<(), StartupError> {
    let filter = parse_filter(&cfg.level)?;
    let (filter_layer, handle) = reload::Layer::new(filter);

    let result = init_inner(cfg, filter_layer);
    if result.is_ok() {
        // 重复初始化时 `set_global_default` 已失败，句柄不必覆盖
        let _ = FILTER.set(handle);
        #[cfg(feature = "log-file")]
        if cfg.file.enabled {
            tracing::info!(
                dir = %cfg.file.dir,
                name = %cfg.file.name,
                rotation = ?cfg.file.rotation,
                mode = ?cfg.file.mode,
                overflow = ?cfg.file.overflow,
                compress = cfg.file.compress,
                max_files = cfg.file.max_files,
                "文件日志已启用"
            );
        }
    }
    result
}

/// 初始化日志、指标与追踪。
///
/// 应在连接资源**之前**调用，以保证资源连接错误可被日志捕获。
///
/// # Errors
/// 日志级别表达式非法或全局 subscriber 已被初始化时返回 [`StartupError::Telemetry`]。
pub fn init_tracing(cfg: &TelemetrySection) -> Result<(), StartupError> {
    init(cfg)
}

/// 运行期调整日志级别（`[telemetry].level` 热更新）。
///
/// 接受与 `RUST_LOG` 相同的表达式（如 `info`、`aurora=debug,tower_http=warn`）。
///
/// # Errors
/// 表达式非法、或日志订阅器尚未初始化（[`init_tracing`] 未调用 / 已失败）时返回错误说明。
pub fn set_level(level: &str) -> Result<(), String> {
    let filter = parse_filter(level).map_err(|e| e.to_string())?;
    let handle = FILTER
        .get()
        .ok_or_else(|| "日志订阅器尚未初始化，无法调整级别".to_string())?;
    // `reload` 内部会重建 interest cache，因此 `enabled!` 之类判定会立刻跟上
    handle
        .reload(filter)
        .map_err(|e| format!("日志级别重载失败：{e}"))
}

/// 解析级别表达式。
fn parse_filter(level: &str) -> Result<EnvFilter, StartupError> {
    EnvFilter::try_new(level)
        .map_err(|e| StartupError::Telemetry(format!("invalid telemetry.level `{level}`: {e}")))
}

/// 组装 subscriber（`filter` 已包在 reload 层里）。
///
/// 用 `Dispatch` 而不是 `Layered::try_init`：otel 层挂在 `Option<Layer>` 上，
/// 启用与未启用因此是**同一个** subscriber 类型，不必为两种组合各写一遍。
fn init_inner(
    cfg: &TelemetrySection,
    filter: reload::Layer<EnvFilter, Registry>,
) -> Result<(), StartupError> {
    // 控制台：`stdout = false` 时换成黑洞。层本身仍挂着——比再拆一条 cfg 分支便宜，
    // 且格式化成本相对 IO 可忽略。
    let console = LogSink(if cfg.stdout {
        Target::Stdout
    } else {
        Target::Sink
    });

    #[cfg(feature = "log-file")]
    let file = LogSink(if cfg.file.enabled {
        Target::File(crate::telemetry::appender::install(&cfg.file)?)
    } else {
        Target::Sink
    });
    #[cfg(not(feature = "log-file"))]
    let file = LogSink(Target::Sink);

    let registry = tracing_subscriber::registry().with(filter);

    // OTel 层用 `Option<Layer>` 表达开/关（`trace::layer` 内部已看 `otel.enabled`）。
    #[cfg(feature = "otel")]
    let registry = {
        let trace = crate::telemetry::trace::layer(&cfg.otel)?;
        let logs = crate::telemetry::trace::logs_layer(&cfg.otel)?;
        registry.with(trace).with(logs)
    };

    // 同一条事件会被格式化两次（控制台 + 文件）——双目的地必然的代价；
    // 只落盘时把 `[telemetry].stdout = false`。文件层固定关掉 ANSI 转义。
    //
    // JSON 走自定义 formatter：`trace_id` 提升到顶层、事件字段平铺（见
    // `telemetry::json_format`）。Pretty 保持 stock 输出——人读场景需要颜色与对齐。
    let json_format = crate::telemetry::json_format::JsonWithTraceId::with_spans();
    let dispatch = match cfg.format {
        LogFormat::Json => Dispatch::new(
            registry
                .with(
                    fmt::layer()
                        .json()
                        .event_format(json_format)
                        .with_writer(console),
                )
                .with(
                    fmt::layer()
                        .json()
                        .event_format(json_format)
                        .with_ansi(false)
                        .with_writer(file),
                ),
        ),
        LogFormat::Pretty => Dispatch::new(
            registry
                .with(fmt::layer().with_writer(console))
                .with(fmt::layer().with_ansi(false).with_writer(file)),
        ),
    };

    tracing::dispatcher::set_global_default(dispatch)
        .map_err(|error| StartupError::Telemetry(error.to_string()))
}
