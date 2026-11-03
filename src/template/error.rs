//! 模板错误（[`TemplateError`]）。
//!
//! 对外**只暴露固定文案**：`AppError` 里放 `50008` 的默认文案，模板名 / 行号 /
//! 引擎原始信息只进日志。
//!
//! 原因不是洁癖：MiniJinja 的错误信息会带上**模板源码片段**，而后台可编辑的
//! 模板里往往混着业务数据（订单号、手机号）。把它原样塞进 500 响应体，
//! 等于给了一个「用错误页读出业务数据」的口子。

use std::fmt;

use crate::errors::{AppError, ErrorCode};

/// 模板错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TemplateErrorKind {
    /// 模板名未注册（`strict` 无关，属于「找不到」而不是「取值失败」）。
    NotFound,
    /// 语法错误：加载期即暴露，不会等到渲染。
    Syntax,
    /// 访问了未定义变量（仅 `strict = true`）。
    Undefined,
    /// 模板来源读取失败（目录不可读、单模板超限、名字非法等）。
    Source,
    /// 其他渲染期错误（过滤器参数、类型不匹配等）。
    Render,
}

/// 模板错误。
pub struct TemplateError {
    kind: TemplateErrorKind,
    /// 出错模板名（环境级错误没有）。
    name: Option<String>,
    /// 出错行号（引擎能给出时）。
    line: Option<usize>,
    /// 明细（**只用于日志**，不要直接回给客户端）。
    message: String,
    /// 原始错误（保留调用链，便于 `tracing` 与 `source()` 回溯）。
    cause: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl TemplateError {
    /// 模板来源读取失败。
    ///
    /// 业务自定义 [`TemplateSource`](crate::template::TemplateSource) 时用它上报
    /// I/O 或反序列化失败。
    #[must_use]
    pub fn load(message: impl Into<String>) -> Self {
        Self {
            kind: TemplateErrorKind::Source,
            name: None,
            line: None,
            message: message.into(),
            cause: None,
        }
    }

    /// 渲染期错误（手工构造，主要给测试与自定义来源使用）。
    #[must_use]
    pub fn render(message: impl Into<String>) -> Self {
        Self {
            kind: TemplateErrorKind::Render,
            name: None,
            line: None,
            message: message.into(),
            cause: None,
        }
    }

    /// 由引擎错误转换；`name` 是本次操作的模板名（引擎自身的 `name()` 可能为空）。
    pub(crate) fn from_minijinja(name: Option<&str>, error: minijinja::Error) -> Self {
        let kind = match error.kind() {
            minijinja::ErrorKind::TemplateNotFound => TemplateErrorKind::NotFound,
            minijinja::ErrorKind::SyntaxError => TemplateErrorKind::Syntax,
            minijinja::ErrorKind::UndefinedError => TemplateErrorKind::Undefined,
            _ => TemplateErrorKind::Render,
        };
        let resolved_name = error
            .name()
            .map(str::to_string)
            .or_else(|| name.map(str::to_string));
        let line = error.line();
        let message = error.to_string();
        Self {
            kind,
            name: resolved_name,
            line,
            message,
            cause: Some(Box::new(error)),
        }
    }

    /// 错误类别。
    #[must_use]
    pub fn kind(&self) -> TemplateErrorKind {
        self.kind
    }

    /// 出错模板名。
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// 出错行号。
    #[must_use]
    pub fn line(&self) -> Option<usize> {
        self.line
    }

    /// 明细（**含模板信息，不要回给客户端**）。
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.message
    }

    /// 是否为「模板不存在」。
    ///
    /// 供业务二次映射：模板名来自 URL（如 CMS 的 `/{slug}`）时，业务应自行转成
    /// `30001`（404），而不是把「页面不存在」表现成 `50008`。框架不猜这个语义。
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.kind == TemplateErrorKind::NotFound
    }
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(name) = &self.name {
            write!(f, "（模板 {name}")?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
            }
            write!(f, "）")?;
        }
        Ok(())
    }
}

impl fmt::Debug for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TemplateError")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("line", &self.line)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl std::error::Error for TemplateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
}

impl From<TemplateError> for AppError {
    /// 只回 `50008` 的固定文案；明细由 [`TemplateEngine`](crate::template::TemplateEngine)
    /// 在转换前写入日志。
    fn from(_error: TemplateError) -> Self {
        Self::biz(
            ErrorCode::TEMPLATE_ERROR,
            ErrorCode::TEMPLATE_ERROR.default_message(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_map_missing_template_to_not_found_kind() {
        let env = minijinja::Environment::new();
        let raw = env
            .get_template("missing.html")
            .expect_err("空环境里任何模板都不存在");
        let error = TemplateError::from_minijinja(Some("missing.html"), raw);

        assert_eq!(error.kind(), TemplateErrorKind::NotFound);
        assert!(error.is_not_found(), "业务据此转 404");
        assert_eq!(error.name(), Some("missing.html"), "应回填调用方给的模板名");
        assert!(error.to_string().contains("missing.html"));
    }

    #[test]
    fn should_not_leak_detail_into_client_error() {
        let error = TemplateError::render("源码片段：{{ secret.token }}");
        let app_error: AppError = error.into();

        assert_eq!(app_error.code(), ErrorCode::TEMPLATE_ERROR);
        let rendered = app_error.to_string();
        assert_eq!(rendered, ErrorCode::TEMPLATE_ERROR.default_message());
        assert!(
            !rendered.contains("secret"),
            "明细（可能含业务数据）不能出现在客户端可见的错误里：{rendered}"
        );
    }

    #[test]
    fn should_report_load_kind_for_custom_source() {
        let error = TemplateError::load("查询 templates 表失败");
        assert_eq!(error.kind(), TemplateErrorKind::Source);
        assert!(!error.is_not_found(), "来源读取失败不等于模板不存在");
    }
}
