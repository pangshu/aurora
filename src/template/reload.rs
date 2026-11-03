//! 重载结果与目录热更新（feature `template`）。
//!
//! 编译失败不再「第一处就返回」：一次重载会把**所有**坏模板收齐再上报——
//! 后台改模板时，运营想看到的是「这 3 个页面第 12 行有问题」，而不是改一个错、
//! 保存一次、再被下一个错误拦住。

use std::fmt;

#[cfg(feature = "template-hot-reload")]
use std::path::Path;

#[cfg(feature = "template-hot-reload")]
use notify::Watcher as _;

use crate::template::error::TemplateError;

/// 防抖窗口：编辑器保存一次常产生多个事件（临时文件 + rename）。
///
/// 与配置热更新的 300 ms 不同取 200 ms：模板目录通常更大、事件更密，
/// 但开发者对「改完立刻刷新」的体感要求更高。
#[cfg(feature = "template-hot-reload")]
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

/// 单个模板的编译失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateFailure {
    name: String,
    line: Option<usize>,
    detail: String,
}

impl TemplateFailure {
    pub(crate) fn new(
        name: impl Into<String>,
        line: Option<usize>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            line,
            detail: detail.into(),
        }
    }

    pub(crate) fn from_error(name: &str, error: &TemplateError) -> Self {
        Self {
            name: name.to_string(),
            line: error.line(),
            detail: error.detail().to_string(),
        }
    }

    /// 模板名。
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 出错行号。
    #[must_use]
    pub fn line(&self) -> Option<usize> {
        self.line
    }

    /// 明细（含引擎原文，**只用于日志 / 后台回显**）。
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for TemplateFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        write!(f, " {}", self.detail)
    }
}

/// 一次**成功**的重载结果。
///
/// 失败不返回本结构：`reload` / `replace` / `upsert` 在编译不通过时直接返回
/// `Err`（旧环境保持不变），避免「返回了报告但其实没生效」这种歧义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReloadReport {
    templates: usize,
}

impl ReloadReport {
    pub(crate) fn new(templates: usize) -> Self {
        Self { templates }
    }

    /// 本次生效的模板数量。
    #[must_use]
    pub fn templates(self) -> usize {
        self.templates
    }
}

/// 模板失败清单 → 人类可读的摘要（最多列 [`MAX_REPORTED`] 条，其余给总数）。
///
/// 上百个模板同时坏掉时不铺满日志/响应体，但也不隐藏「还有更多」。
pub(crate) const MAX_REPORTED: usize = 10;

pub(crate) fn summarize(failures: &[TemplateFailure]) -> String {
    let mut summary = format!("{} 个模板编译失败，模板未生效：", failures.len());
    for (index, failure) in failures.iter().take(MAX_REPORTED).enumerate() {
        if index > 0 {
            summary.push('；');
        }
        summary.push_str(&failure.to_string());
    }
    if failures.len() > MAX_REPORTED {
        summary.push_str(&format!("；……另有 {} 个", failures.len() - MAX_REPORTED));
    }
    summary
}

/// 模板目录监听器（feature `template-hot-reload`）；**drop 即停止**。
///
/// 与 `ConfigWatcher` 同构：监听在独立任务里，句柄必须活着（放进 `AppState`），
/// 否则任务被 abort、监听立即失效。
#[cfg(feature = "template-hot-reload")]
#[derive(Debug)]
pub struct TemplateWatcher {
    task: tokio::task::JoinHandle<()>,
}

#[cfg(feature = "template-hot-reload")]
impl TemplateWatcher {
    /// 监听 `dir` 并在静默 [`DEBOUNCE`] 后重载。
    ///
    /// 必须在 tokio 运行时内调用（内部 `tokio::spawn`）。
    ///
    /// # Errors
    /// 监听器创建失败或目录不可监听时返回 `50005`（启动期可据此决定是否告警）。
    pub(crate) fn start(
        engine: crate::template::TemplateEngine,
        dir: &Path,
    ) -> crate::errors::AppResult<Self> {
        use crate::errors::{AppError, ErrorCode};

        // 只关心「有没有事件」，事件内容不重要：任何变化都触发一次全量重载。
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let mut watcher = notify::recommended_watcher(
            move |event: Result<notify::Event, notify::Error>| {
                if event.is_ok() {
                    let _ = tx.send(());
                }
            },
        )
        .map_err(|error| {
            AppError::biz(
                ErrorCode::SERVICE_UNAVAILABLE,
                format!("模板监听器创建失败：{error}"),
            )
        })?;
        watcher
            .watch(dir, notify::RecursiveMode::Recursive)
            .map_err(|error| {
                AppError::biz(
                    ErrorCode::SERVICE_UNAVAILABLE,
                    format!("监听模板目录失败（{}）：{error}", dir.display()),
                )
            })?;

        let task = tokio::spawn(async move {
            // watcher 必须活到任务结束，否则监听立即失效
            let _watcher = watcher;

            while rx.recv().await.is_some() {
                // 防抖：持续有事件就顺延，静默 DEBOUNCE 后才真正重载
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(DEBOUNCE) => break,
                        Some(_) = rx.recv() => {}
                    }
                }

                // 重载是全量编译（CPU + I/O），不占 async worker
                let engine = engine.clone();
                match tokio::task::spawn_blocking(move || engine.reload()).await {
                    Ok(Ok(report)) => {
                        tracing::info!(templates = report.templates(), "模板已热更新");
                    }
                    // fail-safe：坏模板绝不能让线上失去可用模板
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "模板热更新被拒绝，保留旧版本");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "模板重载任务异常");
                    }
                }
            }
        });

        Ok(Self { task })
    }
}

#[cfg(feature = "template-hot-reload")]
impl Drop for TemplateWatcher {
    fn drop(&mut self) {
        // abort 后任务里的 `_watcher` 随之 drop，底层监听被注销。
        self.task.abort();
    }
}
