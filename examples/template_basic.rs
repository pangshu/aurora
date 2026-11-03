//! template_basic —— 服务端渲染最小示例（feature `template`）。
//!
//! 运行：`cargo run --example template_basic --features template`
//!
//! - `GET /`       → 继承 `layout.html` 的页面，正常渲染
//! - `GET /broken` → 模板引用了上下文未提供的变量，`strict = true` 下返回
//!   **`50008` 的统一 JSON 错误体**（框架不提供 HTML 错误页）
//!
//! 再加上 `template-hot-reload` 还能演示热更新：
//! `cargo run --example template_basic --features template,template-hot-reload`，
//! 运行期间编辑 `examples/templates/index.html`，刷新即生效（静默 200 ms 后原子重载）。
//!
//! 本示例只装配 [`TemplateEngine`]，不拉起完整的 `AppState`：模板与数据库、
//! 缓存无关，重点在「引擎怎么用」。模板文件见 `examples/templates/`。
//!
//! 注意：页面、布局、业务上下文都属应用层职责，框架只提供引擎（`docs/20` §1.2）。

use aurora::config::sections::{TelemetrySection, TemplateSection};
use aurora::prelude::*;
use aurora::template::TemplateEngine;
use axum::extract::State;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Page {
    title: String,
    items: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 渲染失败的明细（模板名 / 行号 / 引擎原文）只进日志，所以先把日志开起来。
    init_tracing(&TelemetrySection::default())?;

    // `dir` 指向示例模板目录（相对进程工作目录 = crate 根）。
    // 生产里通常留空由业务注入，或指向镜像内的模板目录。
    // `TemplateSection` 是 `#[non_exhaustive]`：crate 外只能「取默认值再改字段」。
    let mut config = TemplateSection::default();
    config.dir = "examples/templates".to_string();

    // 启动期全量加载并编译：任一模板语法错误都会在这里失败，而不是等用户访问。
    let engine = TemplateEngine::from_config(&config)?;
    println!("已加载模板：{:?}", engine.names());

    // 目录热更新：改完模板文件不用重启（需 `--features template-hot-reload`）。
    // 必须绑到活到 `main` 结束的变量上——watcher 一 drop，监听立刻停止。
    #[cfg(feature = "template-hot-reload")]
    let _watcher = engine.watch()?;

    let app = Router::new()
        .route("/", get(index))
        .route("/broken", get(broken))
        .with_state(engine);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!("GET http://127.0.0.1:3000/");
    println!("GET http://127.0.0.1:3000/broken  （故意触发 50008）");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 正常渲染：上下文用 `Serialize` 结构体（也可传 `serde_json::json!`）。
async fn index(State(engine): State<TemplateEngine>) -> AppResult<Rendered> {
    engine.render(
        "index.html",
        &Page {
            title: "Aurora".to_string(),
            items: vec!["模板渲染".to_string(), "热更新".to_string()],
        },
    )
}

/// 故意踩错：`broken.html` 引用了上下文里没有的 `missing`。
///
/// `strict = true` → 渲染失败 → `50008`（`Content-Type: application/json`）。
/// 同时控制台会打出模板名与行号——**明细只进日志，不进响应体**。
async fn broken(State(engine): State<TemplateEngine>) -> AppResult<Rendered> {
    engine.render("broken.html", serde_json::json!({}))
}
