//! 模板引擎（[`TemplateEngine`]）。
//!
//! # 为什么是「不可变快照 + 原子替换」
//!
//! `minijinja::Environment` 是 `Send + Sync + Clone` 的，且 `get_template(&self)`
//! 不需要可变借用。于是可以把它整份塞进 [`ArcSwap`]：读路径（渲染）**无锁**，
//! 重载时重建一份新环境、全量编译通过后再 `swap`。
//!
//! 这带来的关键性质：**后台把模板改坏时，线上继续用旧环境**。渲染路径永远不会
//! 读到「一半新一半旧」的模板集合，也不会因为一次语法错误让整站 500。
//!
//! 代价是快照里保留了模板源码（`upsert` 需要「在现有全量上改一个」），
//! 内存约为模板总字节数的 2 倍——模板通常几十 KB 到几 MB，可接受。

use std::fmt;
#[cfg(feature = "template-hot-reload")]
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use arc_swap::ArcSwap;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde::Serialize;

use crate::config::sections::TemplateSection;
use crate::errors::{AppError, AppResult, ErrorCode};
use crate::template::error::TemplateError;
use crate::template::reload::{ReloadReport, TemplateFailure, summarize};
use crate::template::render::Rendered;
use crate::template::source::{FsSource, TemplateSource, validate_name};

/// 一次加载得到的不可变快照：环境 + 名字清单 + 源码。
///
/// 三者放在一起，保证 `names()` / 渲染 / `upsert` 看到的永远是**同一份**集合。
struct Snapshot {
    env: Environment<'static>,
    names: Vec<String>,
    /// 构建本快照所用的源码，供 [`TemplateEngine::upsert`] 做增量修改。
    templates: Vec<(String, String)>,
}

struct Inner {
    snapshot: ArcSwap<Snapshot>,
    /// 文件源（`[template].dir` 为空时为 `None`，此时只能靠 `replace` / `upsert` 注入）。
    source: Option<Arc<dyn TemplateSource>>,
    /// 装配配置：重建环境与启动监听都要用。
    config: TemplateSection,
    /// 串行化重载：后台连点两次「保存」不该并发重建两份环境。
    reload_lock: Mutex<()>,
    /// 最近一次重载失败的原因（明细）。成功即清空。
    last_error: Mutex<Option<String>>,
}

/// 取锁并**忽略中毒**：重载失败不该让后续重载永久不可用——那等于把
/// 「一次坏模板」升级成「模板系统瘫痪」。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 模板引擎（`AppState.template`，feature `template`）。
///
/// 启动期**全量加载并编译**；任一模板语法错误即拒绝启动（fail fast，
/// `docs/20` §12-2）。克隆廉价：内部共享同一份快照。
#[derive(Clone)]
pub struct TemplateEngine {
    inner: Arc<Inner>,
}

impl fmt::Debug for TemplateEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = self.inner.snapshot.load();
        f.debug_struct("TemplateEngine")
            .field("templates", &snapshot.names.len())
            .field("dir", &self.inner.config.dir)
            .field("last_error", &self.last_error())
            .finish_non_exhaustive()
    }
}

impl TemplateEngine {
    /// 按 `[template]` 配置装配。
    ///
    /// `dir` 为空串 → 不加载文件源（模板全部由业务注入），**不是错误**；
    /// `dir` 非空但目录不可读 → 失败。
    ///
    /// # Errors
    /// 目录不可读、模板名非法、语法错误、单模板超限时返回 `50008`，
    /// 错误信息带上**文件名与行号**（启动期报错必须能直接定位）。
    pub fn from_config(config: &TemplateSection) -> AppResult<Self> {
        let source: Option<Arc<dyn TemplateSource>> = if config.dir.trim().is_empty() {
            None
        } else {
            Some(Arc::new(
                FsSource::new(config.dir.as_str())
                    .with_extensions(config.extensions.clone())
                    .with_max_template_bytes(config.max_template_bytes.as_u64()),
            ))
        };
        Self::assemble(config, source)
    }

    /// 用自定义来源装配（模板存 DB / 对象存储时使用）。
    ///
    /// # Errors
    /// 同 [`from_config`](Self::from_config)。
    pub fn from_source(
        config: &TemplateSection,
        source: Arc<dyn TemplateSource>,
    ) -> AppResult<Self> {
        Self::assemble(config, Some(source))
    }

    /// 空引擎：没有模板，任何 `render` 都返回 `50008`。
    ///
    /// 用于测试与「模板全部事后注入」的装配场景。
    #[must_use]
    pub fn memory() -> Self {
        let config = TemplateSection {
            dir: String::new(),
            ..TemplateSection::default()
        };
        // 空集合必然构建成功：`build` 的唯一失败来源是模板本身。
        Self::build(&config, None, Vec::new()).expect("空模板集合不应构建失败")
    }

    /// 渲染模板。
    ///
    /// `ctx` 为任意 `Serialize` 值（结构体或 `serde_json::json!`）。
    ///
    /// # Errors
    /// 模板未注册 / 语法错误 / `strict` 下的未定义变量 → `50008`。
    /// 返回给客户端的只有固定文案，明细写日志（见 [`TemplateError`]）。
    pub fn render<V: Serialize>(&self, name: &str, ctx: V) -> AppResult<Rendered> {
        let snapshot = self.inner.snapshot.load();
        let template = snapshot
            .env
            .get_template(name)
            .map_err(|error| Self::render_error(name, error))?;
        // 3.0 起渲染只走 `Into<Value>`，serde 值要经 `value::Serde` 显式包装
        // （`Value::from_serialize` 已移除）。
        let body = template
            .render(minijinja::value::Serde(&ctx))
            .map_err(|error| Self::render_error(name, error))?;
        Ok(Rendered::new(body))
    }

    /// 渲染模板（上下文已是 [`Value`](minijinja::Value)）。
    ///
    /// 适合上下文由多段拼装的场景，避免重复序列化。
    ///
    /// # Errors
    /// 同 [`render`](Self::render)。
    pub fn render_value(&self, name: &str, ctx: minijinja::Value) -> AppResult<Rendered> {
        let snapshot = self.inner.snapshot.load();
        let template = snapshot
            .env
            .get_template(name)
            .map_err(|error| Self::render_error(name, error))?;
        let body = template
            .render(ctx)
            .map_err(|error| Self::render_error(name, error))?;
        Ok(Rendered::new(body))
    }

    /// 从模板源重新拉全量并原子切换。
    ///
    /// **同步阻塞**（读盘 + 全量编译）：在 async 上下文里由业务自行包
    /// `tokio::task::spawn_blocking`；内置的目录监听就是这么做的。
    ///
    /// 无文件源（`dir` 为空）时是空操作，返回当前模板数。
    ///
    /// # Errors
    /// 来源读取失败，或**任一模板编译失败**（旧环境保持不变，错误信息含全部失败清单）。
    pub fn reload(&self) -> AppResult<ReloadReport> {
        let Some(source) = &self.inner.source else {
            tracing::debug!("[template].dir 为空，reload 为空操作");
            return Ok(ReloadReport::new(self.len()));
        };
        let templates = source.load().map_err(|error| load_error(&error))?;
        self.apply(templates)
    }

    /// 用业务给出的**全量**模板原子替换。
    ///
    /// 适合「模板存 DB，后台保存后整份推送」：读取数据失败时业务自己决定是重试
    /// 还是保持旧版本，框架只负责「编译通过才生效」。
    ///
    /// # Errors
    /// 同 [`reload`](Self::reload)。
    pub fn replace(&self, templates: Vec<(String, String)>) -> AppResult<ReloadReport> {
        self.apply(templates)
    }

    /// 新增或更新**单个**模板并原子替换（后台保存一个页面后调用）。
    ///
    /// 在当前快照的全量源码上改这一个，再整体重建——因此不存在「半个环境」。
    ///
    /// # Errors
    /// 模板名非法，或该模板编译失败（旧环境保持不变）。
    pub fn upsert(&self, name: &str, source: &str) -> AppResult<ReloadReport> {
        validate_name(name).map_err(|error| {
            AppError::biz(ErrorCode::TEMPLATE_ERROR, format!("模板名非法：{error}"))
        })?;
        let mut templates = self.inner.snapshot.load().templates.clone();
        match templates.iter_mut().find(|(existing, _)| existing == name) {
            Some((_, slot)) => *slot = source.to_string(),
            None => templates.push((name.to_string(), source.to_string())),
        }
        self.apply(templates)
    }

    /// 最近一次重载失败的原因（成功后被清空）。
    ///
    /// 给健康检查 / 后台页脚展示用；渲染路径不读它。
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        lock(&self.inner.last_error).clone()
    }

    /// 已注册的模板名（字典序）。
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.inner.snapshot.load().names.clone()
    }

    /// 模板数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.snapshot.load().names.len()
    }

    /// 是否没有任何模板。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 监听模板目录，静默 200 ms 后自动重载（feature `template-hot-reload`）。
    ///
    /// 返回的 [`TemplateWatcher`](crate::template::TemplateWatcher) **必须被持有**：
    /// drop 即停止监听。`[template].hot_reload = true` 时 `AppState` 会自动持有。
    ///
    /// # Errors
    /// `dir` 为空、监听器创建失败或目录不可监听时返回错误。
    #[cfg(feature = "template-hot-reload")]
    pub fn watch(&self) -> AppResult<crate::template::TemplateWatcher> {
        let dir = self.inner.config.dir.trim();
        if dir.is_empty() {
            return Err(AppError::biz(
                ErrorCode::TEMPLATE_ERROR,
                "[template].dir 为空，没有可监听的模板目录".to_string(),
            ));
        }
        crate::template::TemplateWatcher::start(self.clone(), Path::new(dir))
    }

    fn assemble(
        config: &TemplateSection,
        source: Option<Arc<dyn TemplateSource>>,
    ) -> AppResult<Self> {
        let templates = match &source {
            Some(source) => source.load().map_err(|error| load_error(&error))?,
            None => Vec::new(),
        };
        Self::build(config, source, templates)
    }

    fn build(
        config: &TemplateSection,
        source: Option<Arc<dyn TemplateSource>>,
        templates: Vec<(String, String)>,
    ) -> AppResult<Self> {
        let snapshot = build_snapshot(config, templates).map_err(compile_error)?;
        Ok(Self {
            inner: Arc::new(Inner {
                snapshot: ArcSwap::from_pointee(snapshot),
                source,
                config: config.clone(),
                reload_lock: Mutex::new(()),
                last_error: Mutex::new(None),
            }),
        })
    }

    /// 原子切换的唯一入口：**编译全部通过才 swap**，否则保留旧环境。
    fn apply(&self, templates: Vec<(String, String)>) -> AppResult<ReloadReport> {
        // 串行化：并发重载不仅浪费 CPU，还会让「谁最后写」变得不可预期。
        let _guard = lock(&self.inner.reload_lock);
        match build_snapshot(&self.inner.config, templates) {
            Ok(snapshot) => {
                let count = snapshot.names.len();
                self.inner.snapshot.store(Arc::new(snapshot));
                *lock(&self.inner.last_error) = None;
                tracing::info!(templates = count, "模板已重载（原子切换）");
                Ok(ReloadReport::new(count))
            }
            Err(failures) => {
                let summary = summarize(&failures);
                *lock(&self.inner.last_error) = Some(summary.clone());
                tracing::warn!(
                    failed = failures.len(),
                    detail = %summary,
                    "模板重载失败，保留旧版本"
                );
                Err(AppError::biz(ErrorCode::TEMPLATE_ERROR, summary))
            }
        }
    }

    /// 渲染期错误：明细进日志，客户端只拿固定文案。
    fn render_error(requested: &str, error: minijinja::Error) -> AppError {
        let error = TemplateError::from_minijinja(Some(requested), error);
        tracing::error!(
            // 入口模板与实际出错的位置可能不是一个文件（`extends` / `include`），
            // 两个都记：`template` 指向真正报错的那个，`requested` 是本次请求的入口。
            requested,
            template = error.name().unwrap_or(requested),
            line = error.line(),
            detail = error.detail(),
            "模板渲染失败"
        );
        AppError::from(error)
    }
}

/// 装载期错误（读目录 / 查库失败）：**保留明细**——那时没有客户端在等响应，
/// 而启动/后台重载的排障完全依赖这条信息。
fn load_error(error: &TemplateError) -> AppError {
    AppError::biz(ErrorCode::TEMPLATE_ERROR, format!("模板加载失败：{error}"))
}

fn compile_error(failures: Vec<TemplateFailure>) -> AppError {
    AppError::biz(ErrorCode::TEMPLATE_ERROR, summarize(&failures))
}

fn build_snapshot(
    config: &TemplateSection,
    templates: Vec<(String, String)>,
) -> Result<Snapshot, Vec<TemplateFailure>> {
    let mut env = Environment::new();
    env.set_undefined_behavior(if config.strict {
        UndefinedBehavior::Strict
    } else {
        UndefinedBehavior::Lenient
    });
    // 引擎默认 500；配置已校验只允许下调（抬高会让栈溢出风险失控）。
    env.set_recursion_limit(config.recursion_limit);
    if !config.autoescape {
        env.set_auto_escape_callback(|_| AutoEscape::None);
    }

    let mut names = Vec::with_capacity(templates.len());
    // 不「第一处就返回」：一次收齐，后台才能一次看全需要修哪些页面。
    let mut failures = Vec::new();
    for (name, source) in &templates {
        if let Err(error) = validate_name(name) {
            failures.push(TemplateFailure::new(
                name.clone(),
                None,
                error.detail().to_string(),
            ));
            continue;
        }
        // `add_template_owned` 会做完整语法解析：写错的花括号在这里就炸，
        // 而不是等到第一个用户访问该页面。
        match env.add_template_owned(name.clone(), source.clone()) {
            Ok(()) => names.push(name.clone()),
            Err(error) => {
                let error = TemplateError::from_minijinja(Some(name), error);
                failures.push(TemplateFailure::from_error(name, &error));
            }
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    names.sort();
    Ok(Snapshot {
        env,
        names,
        templates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::source::MemorySource;

    fn section() -> TemplateSection {
        // 空 dir：单测不碰文件系统
        TemplateSection {
            dir: String::new(),
            ..TemplateSection::default()
        }
    }

    fn engine_with(templates: &[(&str, &str)]) -> TemplateEngine {
        let mut source = MemorySource::new();
        for (name, body) in templates {
            source = source.with(*name, *body);
        }
        TemplateEngine::from_source(&section(), Arc::new(source)).expect("装配应成功")
    }

    /// 可改内容的模板源，用来测 `reload`（不必碰文件系统）。
    #[derive(Clone, Default)]
    struct CannedSource {
        templates: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl CannedSource {
        fn new(pairs: &[(&str, &str)]) -> Self {
            let source = Self::default();
            source.set(pairs);
            source
        }

        fn set(&self, pairs: &[(&str, &str)]) {
            *lock(&self.templates) = pairs
                .iter()
                .map(|(name, body)| ((*name).to_string(), (*body).to_string()))
                .collect();
        }
    }

    impl TemplateSource for CannedSource {
        fn load(&self) -> Result<Vec<(String, String)>, TemplateError> {
            Ok(lock(&self.templates).clone())
        }
    }

    #[test]
    fn should_render_with_inheritance() {
        let engine = engine_with(&[
            ("layout.html", "L:{% block body %}{% endblock %}"),
            (
                "page.html",
                "{% extends \"layout.html\" %}{% block body %}hi {{ name }}{% endblock %}",
            ),
        ]);
        let rendered = engine
            .render("page.html", serde_json::json!({ "name": "aurora" }))
            .expect("渲染应成功");
        assert_eq!(rendered.body(), "L:hi aurora");
    }

    #[test]
    fn should_escape_html_by_default() {
        let engine = engine_with(&[("page.html", "{{ value }}")]);
        let rendered = engine
            .render(
                "page.html",
                serde_json::json!({ "value": "<script>alert(1)</script>" }),
            )
            .expect("渲染应成功");
        assert!(
            !rendered.body().contains("<script>"),
            "默认必须转义，实际：{}",
            rendered.body()
        );
        assert!(rendered.body().contains("&lt;script&gt;"));
    }

    #[test]
    fn should_fail_on_undefined_variable_when_strict() {
        let engine = engine_with(&[("page.html", "{{ missing.field }}")]);
        let error = engine
            .render("page.html", serde_json::json!({}))
            .expect_err("strict 下未定义变量必须报错");
        assert_eq!(error.code(), ErrorCode::TEMPLATE_ERROR);
        assert_eq!(
            error.to_string(),
            ErrorCode::TEMPLATE_ERROR.default_message(),
            "客户端只应看到固定文案"
        );
    }

    #[test]
    fn should_render_empty_when_lenient() {
        let config = TemplateSection {
            strict: false,
            ..section()
        };
        let source = MemorySource::new().with("page.html", "a{{ missing }}b");
        let engine = TemplateEngine::from_source(&config, Arc::new(source)).expect("装配应成功");
        let rendered = engine
            .render("page.html", serde_json::json!({}))
            .expect("lenient 下不应报错");
        assert_eq!(rendered.body(), "ab");
    }

    #[test]
    fn should_reject_syntax_error_at_load_time() {
        let source = MemorySource::new().with("broken.html", "{% if x %}");
        let error = TemplateEngine::from_source(&section(), Arc::new(source))
            .expect_err("语法错误必须在装配期暴露");
        assert_eq!(error.code(), ErrorCode::TEMPLATE_ERROR);
        assert!(
            error.to_string().contains("broken.html"),
            "装配期错误必须带文件名，否则没法排障：{error}"
        );
    }

    #[test]
    fn should_report_not_registered_template() {
        let engine = engine_with(&[("page.html", "hi")]);
        let error = engine
            .render("missing.html", serde_json::json!({}))
            .expect_err("未注册的模板应报错");
        assert_eq!(error.code(), ErrorCode::TEMPLATE_ERROR);
    }

    #[test]
    fn should_expose_sorted_names() {
        let engine = engine_with(&[("b.html", "b"), ("a.html", "a"), ("c/d.html", "d")]);
        assert_eq!(engine.names(), vec!["a.html", "b.html", "c/d.html"]);
        assert_eq!(engine.len(), 3);
        assert!(!engine.is_empty());
    }

    #[test]
    fn should_be_empty_when_memory_engine() {
        let engine = TemplateEngine::memory();
        assert!(engine.is_empty());
        assert_eq!(
            engine
                .render("any.html", serde_json::json!({}))
                .expect_err("空引擎渲染必然失败")
                .code(),
            ErrorCode::TEMPLATE_ERROR
        );
    }

    #[test]
    fn should_allow_empty_dir_without_file_source() {
        // docs/20 §12-1：dir 为空 = 纯业务注入，启动必须成功
        let engine = TemplateEngine::from_config(&section()).expect("空 dir 不应报错");
        assert!(engine.is_empty(), "没有文件源时模板集合为空");
    }

    #[test]
    fn should_fail_when_configured_dir_missing() {
        // docs/20 §12-2：dir 非空但目录不存在 → fail fast
        let config = TemplateSection {
            dir: "definitely-not-exists-dir".to_string(),
            ..section()
        };
        let error = TemplateEngine::from_config(&config).expect_err("目录缺失必须失败");
        assert!(
            error.to_string().contains("模板目录不可读"),
            "实际：{error}"
        );
    }

    #[test]
    fn should_reload_from_source() {
        let source = Arc::new(CannedSource::new(&[("page.html", "v1")]));
        let engine = TemplateEngine::from_source(&section(), source.clone()).expect("装配应成功");
        assert_eq!(
            engine
                .render("page.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "v1"
        );

        source.set(&[("page.html", "v2"), ("new.html", "fresh")]);
        let report = engine.reload().expect("重载应成功");
        assert_eq!(report.templates(), 2);
        assert_eq!(
            engine
                .render("page.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "v2"
        );
        assert_eq!(
            engine
                .render("new.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "fresh",
            "新增的模板应一并生效"
        );
    }

    #[test]
    fn should_keep_old_env_when_reload_is_broken() {
        let source = Arc::new(CannedSource::new(&[("page.html", "v1")]));
        let engine = TemplateEngine::from_source(&section(), source.clone()).expect("装配应成功");

        source.set(&[("page.html", "{% if x %}")]);
        let error = engine.reload().expect_err("坏模板必须被拒绝");
        assert!(error.to_string().contains("page.html"), "实际：{error}");

        assert_eq!(
            engine
                .render("page.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "v1",
            "编译失败时线上必须继续用旧版本"
        );
        assert!(
            engine.last_error().is_some(),
            "失败原因要留下，供后台/健康检查展示"
        );
    }

    #[test]
    fn should_report_all_failures_at_once() {
        let engine = engine_with(&[("page.html", "ok")]);
        let error = engine
            .replace(vec![
                ("a.html".to_string(), "{% if x %}".to_string()),
                ("b.html".to_string(), "{% for %}".to_string()),
            ])
            .expect_err("两个坏模板都应被拒绝");
        let message = error.to_string();
        assert!(message.contains("a.html"), "实际：{message}");
        assert!(message.contains("b.html"), "实际：{message}");
        assert!(
            message.contains("2 个"),
            "应给出失败总数，便于后台一次修完：{message}"
        );
    }

    #[test]
    fn should_upsert_single_template() {
        let engine = engine_with(&[("page.html", "v1"), ("other.html", "o")]);

        engine.upsert("page.html", "v2").expect("更新应成功");
        assert_eq!(
            engine
                .render("page.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "v2"
        );
        assert_eq!(
            engine
                .render("other.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "o",
            "upsert 不应影响其它模板"
        );

        engine.upsert("new.html", "n").expect("新增应成功");
        assert_eq!(engine.names(), vec!["new.html", "other.html", "page.html"]);
    }

    #[test]
    fn should_reject_broken_upsert_and_keep_old() {
        let engine = engine_with(&[("page.html", "v1")]);
        let error = engine
            .upsert("page.html", "{% endif %}")
            .expect_err("坏模板必须被拒绝");
        assert!(error.to_string().contains("page.html"));
        assert_eq!(
            engine
                .render("page.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "v1"
        );
    }

    #[test]
    fn should_reject_invalid_name_on_upsert() {
        let engine = engine_with(&[("page.html", "v1")]);
        let error = engine
            .upsert("../escape.html", "x")
            .expect_err("路径穿越必须被拒绝");
        assert!(error.to_string().contains("模板名非法"), "实际：{error}");
    }

    #[test]
    fn should_survive_concurrent_reads_during_replace() {
        let engine = engine_with(&[("page.html", "v1")]);

        let reader = engine.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..500 {
                let body = reader
                    .render("page.html", serde_json::json!({}))
                    .expect("读到的快照永远是完整可渲染的")
                    .into_body();
                assert!(
                    body == "v1" || body.starts_with("v2-"),
                    "不允许读到半成品：{body}"
                );
            }
        });

        for index in 0..50 {
            engine
                .replace(vec![("page.html".to_string(), format!("v2-{index}"))])
                .expect("替换应成功");
        }
        handle.join().expect("读线程不应 panic");
    }

    /// 防抖窗口内自动重载（真实文件系统 + notify）。
    #[cfg(feature = "template-hot-reload")]
    #[tokio::test]
    async fn should_reload_when_watched_file_changes() {
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let dir = TempDir(std::env::temp_dir().join(format!(
            "aurora-template-watch-{}",
            crate::utils::Id::uuid()
        )));
        std::fs::create_dir_all(&dir.0).expect("创建临时目录");
        std::fs::write(dir.0.join("page.html"), "v1").expect("写入模板");

        let mut config = section();
        config.dir = dir.0.to_string_lossy().into_owned();
        let engine = TemplateEngine::from_config(&config).expect("装配应成功");
        assert_eq!(
            engine
                .render("page.html", serde_json::json!({}))
                .unwrap()
                .body(),
            "v1"
        );

        let watcher = engine.watch().expect("监听应成功");
        std::fs::write(dir.0.join("page.html"), "v2").expect("改写模板");

        let mut reloaded = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let body = engine
                .render("page.html", serde_json::json!({}))
                .map(Rendered::into_body)
                .unwrap_or_default();
            if body == "v2" {
                reloaded = true;
                break;
            }
        }

        drop(watcher);
        assert!(reloaded, "文件变化后应在防抖窗口内自动重载");
    }
}
