//! 模板渲染（feature `template`）。
//!
//! 引擎为 **MiniJinja**（Jinja2 语法兼容），封装为 [`TemplateEngine`]，
//! 经 `AppState.template` 注入。设计见 `docs/20-template-engine.md`。
//!
//! - [`TemplateEngine`]：引擎（启动期全量加载并编译，渲染错误映射 `50008`）
//! - [`Rendered`]：`text/html; charset=utf-8` 响应
//! - [`TemplateError`]：渲染错误（对外只暴露固定文案，明细进日志）
//! - [`TemplateSource`] / [`FsSource`] / [`MemorySource`]：模板来源
//! - [`validate_name`]：模板名校验（路径穿越防线）
//!
//! # 边界
//!
//! - **不提供 HTML 错误页**：渲染失败仍返回统一 JSON 错误体（`50008`），
//!   与 `docs/09` §1 的响应契约一致；
//! - **不提供静态资源 serving**：CSS / JS / 图片由应用层或前置反代负责；
//! - **不提供具体页面**：布局、组件、业务上下文全部属于应用层。
//!
//! # 安全前提
//!
//! 「后台能编辑模板」≈「后台能发布 HTML/JS」。本模块不解决权限模型，
//! 只做默认转义、体积与递归上限、模板名校验。模板编辑权限必须由应用层
//! 按**发布权限**对待（`docs/20` §9）。

mod engine;
mod error;
#[cfg(feature = "template-i18n")]
mod i18n;
mod reload;
mod render;
mod source;

pub use engine::TemplateEngine;
pub use error::{TemplateError, TemplateErrorKind};
#[cfg(feature = "template-hot-reload")]
pub use reload::TemplateWatcher;
pub use reload::{ReloadReport, TemplateFailure};
pub use render::Rendered;
pub use source::{FsSource, MemorySource, TemplateSource, validate_name};
