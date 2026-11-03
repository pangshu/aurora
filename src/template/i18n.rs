//! 模板内的 fluent 翻译（feature `template-i18n`）。
//!
//! # 为什么不是「把 Translator 塞进上下文」
//!
//! `Translator` 内部是 `FluentBundle`，**非 `Send`**，因此 `i18n` 模块把按语言的
//! 翻译器缓存在 `thread_local!` 的 `Rc` 里，只对外暴露 `with_translator(闭包借用)`。
//! 而 `minijinja::Value` 要求对象 `Send + Sync + 'static`——`&Translator` 根本装不进去。
//!
//! 所以这里注入的是**按需查询的函数**：`t()` 捕获 `(I18n, Locale)`（两者都是
//! `Clone + Send + Sync`），每次调用走一次 `I18n::t_args`（thread-local 查表 + 格式化）。
//! 一次页面渲染几十个翻译对性能影响可忽略，换来的是不用为模板再造一套 i18n 缓存。
//!
//! 用法：
//!
//! ```jinja
//! {{ t("hello") }}
//! {{ t("welcome", name = user.name) }}
//! ```

use std::fmt;
use std::sync::Arc;

use fluent::FluentArgs;
use minijinja::Value;
use minijinja::value::{Kwargs, Object, Serde};
use serde::Serialize;

use crate::errors::AppResult;
use crate::i18n::{I18n, Locale};
use crate::template::engine::TemplateEngine;
use crate::template::render::Rendered;

impl TemplateEngine {
    /// 渲染模板，并把 fluent 的 `t()` 注入上下文（feature `template-i18n`）。
    ///
    /// `locale` 通常来自 `Locale` 提取器（已由 `middleware/locale` 协商一次）；
    /// 业务变量的优先级高于 `t`：上下文里自带 `t` 时以业务为准。
    ///
    /// # Errors
    /// 同 [`render`](TemplateEngine::render)。
    pub fn render_i18n<V: Serialize>(
        &self,
        name: &str,
        ctx: V,
        i18n: &I18n,
        locale: &Locale,
    ) -> AppResult<Rendered> {
        let base: Value = Serde(&ctx).into();
        let merged = Value::from_object(LocalizedContext {
            base,
            translator: translator_function(i18n.clone(), locale.clone()),
        });
        self.render_value(name, merged)
    }
}

/// 上下文包装：业务变量 + `t()`。
struct LocalizedContext {
    base: Value,
    translator: Value,
}

impl fmt::Debug for LocalizedContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Value` 的 Debug 会把整个上下文打出来（可能含业务数据），只标类型即可。
        f.debug_struct("LocalizedContext").finish_non_exhaustive()
    }
}

impl Object for LocalizedContext {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        // 业务上下文优先：显式传进来的 `t` 覆盖框架注入的那个（可测 / 可替换）。
        if let Ok(value) = self.base.get_item(key)
            && !value.is_undefined()
        {
            return Some(value);
        }
        if key.as_str() == Some("t") {
            return Some(self.translator.clone());
        }
        None
    }
}

/// 构造 `t(key, **kwargs)` 函数值。
fn translator_function(i18n: I18n, locale: Locale) -> Value {
    Value::from_function(move |key: String, kwargs: Kwargs| -> String {
        let mut args = FluentArgs::new();
        for name in kwargs.args() {
            // fluent 的占位符是 `{ $name }`：这里把模板里的 `name = ...` 逐个搬过去。
            // 取不到（类型不支持）就跳过，而不是让整页渲染失败——翻译是附属信息，
            // 不该因为它挂掉整个页面。
            if let Ok(value) = kwargs.get::<String>(name) {
                args.set(name.to_string(), value);
            }
        }
        i18n.t_args(&locale, &key, &args)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::sections::I18nSection;
    use crate::config::sections::TemplateSection;
    use crate::template::source::MemorySource;

    fn engine(template: &str) -> TemplateEngine {
        let config = TemplateSection {
            dir: String::new(), // 不碰文件系统
            ..TemplateSection::default()
        };
        let source = MemorySource::new().with("page.html", template);
        TemplateEngine::from_source(&config, Arc::new(source)).expect("装配应成功")
    }

    fn i18n() -> I18n {
        let config = I18nSection {
            // 复用仓库里的示例语言资源（`examples/locales/{zh-CN,en}.ftl`）
            resource_dir: "examples/locales".to_string(),
            ..I18nSection::default()
        };
        I18n::from_config(&config).expect("加载语言资源应成功")
    }

    fn locale(raw: &str) -> Locale {
        Locale::parse(raw).expect("合法语言标识")
    }

    #[test]
    fn should_translate_without_arguments() {
        let engine = engine(r#"{{ t("hello") }}"#);
        let i18n = i18n();

        assert_eq!(
            engine
                .render_i18n("page.html", serde_json::json!({}), &i18n, &locale("zh-CN"))
                .unwrap()
                .into_body(),
            "你好，世界"
        );
        assert_eq!(
            engine
                .render_i18n("page.html", serde_json::json!({}), &i18n, &locale("en"))
                .unwrap()
                .into_body(),
            "Hello, world"
        );
    }

    #[test]
    fn should_render_negotiated_locale() {
        // 真实链路：Accept-Language → 协商（`middleware/locale` 做的就是这件事）→ 渲染
        let engine = engine(r#"{{ t("hello") }}|{{ t("welcome", name = user) }}"#);
        let i18n = i18n();
        let ctx = serde_json::json!({ "user": "Aurora" });

        let zh = i18n.negotiate(Some("zh-CN,zh;q=0.9,en;q=0.8"));
        assert_eq!(zh.as_str(), "zh-CN");
        assert_eq!(
            engine
                .render_i18n("page.html", &ctx, &i18n, &zh)
                .unwrap()
                .into_body(),
            "你好，世界|欢迎，Aurora！"
        );

        let en = i18n.negotiate(Some("en-US,en;q=0.9"));
        assert_eq!(en.as_str(), "en");
        assert_eq!(
            engine
                .render_i18n("page.html", &ctx, &i18n, &en)
                .unwrap()
                .into_body(),
            "Hello, world|Welcome, Aurora!"
        );
    }

    #[test]
    fn should_pass_named_arguments_to_fluent() {
        let engine = engine(r#"{{ t("welcome", name = user) }}"#);
        let i18n = i18n();
        let ctx = serde_json::json!({ "user": "Aurora" });

        assert_eq!(
            engine
                .render_i18n("page.html", &ctx, &i18n, &locale("zh-CN"))
                .unwrap()
                .into_body(),
            "欢迎，Aurora！"
        );
        assert_eq!(
            engine
                .render_i18n("page.html", &ctx, &i18n, &locale("en"))
                .unwrap()
                .into_body(),
            "Welcome, Aurora!"
        );
    }

    #[test]
    fn should_keep_business_variables() {
        // 注入 `t` 不能挤掉业务变量：同一个模板里两者都要能用
        let engine = engine(r#"{{ t("hello") }} / {{ user }}"#);
        let rendered = engine
            .render_i18n(
                "page.html",
                serde_json::json!({ "user": "Aurora" }),
                &i18n(),
                &locale("zh-CN"),
            )
            .expect("渲染应成功");
        assert_eq!(rendered.into_body(), "你好，世界 / Aurora");
    }

    #[test]
    fn should_return_key_when_translation_missing() {
        // 翻译缺失不报错：Translator 的约定是「原样返回 key」，便于发现漏翻
        let engine = engine(r#"{{ t("not.translated") }}"#);
        let rendered = engine
            .render_i18n(
                "page.html",
                serde_json::json!({}),
                &i18n(),
                &locale("zh-CN"),
            )
            .expect("缺翻译不应让页面失败");
        assert_eq!(rendered.into_body(), "not.translated");
    }

    #[test]
    fn should_escape_translated_html() {
        // 翻译文本同样走自动转义：.ftl 里的尖括号不能变成标签
        let engine = engine(r#"{{ t("hello") }}"#);
        let rendered = engine
            .render_i18n(
                "page.html",
                serde_json::json!({}),
                &i18n(),
                &locale("zh-CN"),
            )
            .unwrap();
        assert!(!rendered.body().contains("<script"), "翻译结果也必须转义");
    }

    #[test]
    fn should_fallback_to_default_locale_resources() {
        // 未提供的语言：I18n 会回退到默认/兜底资源的 key 查找，
        // 这里只断言「不 panic 且拿到字符串」，具体回退策略属 i18n 模块的职责
        let engine = engine(r#"{{ t("hello") }}"#);
        let rendered = engine
            .render_i18n("page.html", serde_json::json!({}), &i18n(), &locale("ja"))
            .expect("未知语言不应 panic");
        assert!(!rendered.body().is_empty());
    }
}
