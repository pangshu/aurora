//! `[template]` 配置段（feature `template`）。
//!
//! 字段与默认值见 `docs/08-config-spec.md` §3 `[template]`；设计与取舍见 `docs/20`。
//!
//! 两条容易写错的约束（`docs/20` §12）：
//! - `dir` 为**空串**表示「不加载文件源」，模板全部由业务注入，启动**不报错**；
//! - `dir` 非空但目录不可读 → **启动失败**（fail fast），不会降级成空引擎。

use byte_unit::Byte;
use serde::Deserialize;

use crate::config::sections::de::de_byte;

/// 模板引擎配置段。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TemplateSection {
    /// 模板根目录（相对进程工作目录或绝对路径）。
    ///
    /// **空字符串 = 不加载文件源**：模板全部由应用层经
    /// [`TemplateEngine`](crate::template::TemplateEngine) 注入（`docs/20` §12-1）。
    pub dir: String,
    /// 参与加载的扩展名白名单（不含点，大小写不敏感）。
    ///
    /// 只收录这些后缀，避免把 `.map` / `.swp` 之类的伴生文件当模板加载。
    pub extensions: Vec<String>,
    /// 运行期监听目录变化（feature `template-hot-reload`）。
    ///
    /// 生产应保持 `false`：容器内的模板改动应走「后台编辑 → `upsert`/`replace`」，
    /// 而不是改文件；开发期打开即可边改边看。
    ///
    /// 启用该 feature 但保持 `false` 时完全不启动监听线程/任务。
    pub hot_reload: bool,
    /// 未定义变量即报错（推荐）。
    ///
    /// `false` 时与 Jinja2 默认行为一致：渲染成空串。对后台可编辑的模板，
    /// 空串会静默产出「少了一段内容」的页面，比 500 更难发现。
    pub strict: bool,
    /// HTML 自动转义。
    ///
    /// 关闭等于允许后台模板注入任意脚本——仅在模板内容完全可信时使用。
    pub autoescape: bool,
    /// 递归上限（含 `include` / `extends` / 宏展开）。
    ///
    /// 引擎默认 500，本项目**只允许下调**：上限抬高后栈溢出风险不可控。
    pub recursion_limit: usize,
    /// 单个模板体积上限（后台粘贴巨型文件 / 误提交产物文件的兜底）。
    #[serde(deserialize_with = "de_byte")]
    pub max_template_bytes: Byte,
}

impl Default for TemplateSection {
    fn default() -> Self {
        Self {
            dir: "templates".to_string(),
            extensions: vec![
                "html".to_string(),
                "htm".to_string(),
                "jinja".to_string(),
                "jinja2".to_string(),
            ],
            hot_reload: false,
            strict: true,
            autoescape: true,
            recursion_limit: 500,
            max_template_bytes: Byte::from_u64(1024 * 1024),
        }
    }
}

impl TemplateSection {
    /// 启动期校验。
    ///
    /// # Errors
    /// 目录含 `..`、扩展名非法或为空、递归上限越界、单模板上限为 0 时返回错误信息。
    pub fn validate(&self) -> Result<(), String> {
        if self.dir.contains("..") {
            return Err(format!(
                "template.dir = \"{}\" 不允许包含 `..`：模板根目录必须是确定的一层根",
                self.dir
            ));
        }
        if self.extensions.is_empty() {
            return Err("template.extensions 不能为空：至少给一个扩展名".to_string());
        }
        for extension in &self.extensions {
            let normalized = extension.trim().trim_start_matches('.');
            if normalized.is_empty() {
                return Err("template.extensions 含有空扩展名".to_string());
            }
            if normalized.contains(['/', '\\', '*', '?']) || normalized.contains("..") {
                return Err(format!(
                    "template.extensions 的 \"{extension}\" 不是合法扩展名"
                ));
            }
        }
        // 引擎默认上限即 500；超过它会被静默截断，等于「配了不生效」，故直接拒绝。
        if self.recursion_limit == 0 || self.recursion_limit > 500 {
            return Err(format!(
                "template.recursion_limit = {} 超出允许区间 1..=500",
                self.recursion_limit
            ));
        }
        if self.max_template_bytes.as_u64() == 0 {
            return Err("template.max_template_bytes 必须大于 0".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_default_to_strict_and_escaped() {
        let section = TemplateSection::default();
        assert!(section.strict, "未定义变量默认应报错");
        assert!(section.autoescape, "默认必须开启 HTML 转义");
        assert!(!section.hot_reload, "生产默认不监听目录");
        assert_eq!(section.dir, "templates");
        assert_eq!(section.max_template_bytes.as_u64(), 1024 * 1024);
        section.validate().expect("默认配置必须自洽");
    }

    #[test]
    fn should_allow_empty_dir_for_injected_templates() {
        // 空目录 = 纯业务注入（docs/20 §12-1），不是配置错误
        let section = TemplateSection {
            dir: String::new(),
            ..TemplateSection::default()
        };
        section.validate().expect("空 dir 应允许");
    }

    #[test]
    fn should_reject_escaping_dir() {
        let section = TemplateSection {
            dir: "../templates".to_string(),
            ..TemplateSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains(".."), "应指出非法片段：{error}");
    }

    #[test]
    fn should_reject_out_of_range_recursion_limit() {
        let section = TemplateSection {
            recursion_limit: 501,
            ..TemplateSection::default()
        };
        assert!(
            section.validate().is_err(),
            "超过引擎默认上限会被静默截断，必须在配置期拒绝"
        );
    }

    #[test]
    fn should_reject_empty_extensions() {
        let section = TemplateSection {
            extensions: Vec::new(),
            ..TemplateSection::default()
        };
        assert!(section.validate().is_err());
    }

    #[test]
    fn should_parse_byte_size_from_string() {
        // 走真实的配置形状（`[template]` 段）并反序列化到 `AppConfig`：
        // 顺带验证该段确实注册进了顶层配置，而不是个「写了没人读」的孤儿字段。
        //
        // 单位注意：`byte-unit` 的 `MB` 是十进制 10^6，`MiB` 才是 2^20。
        // 默认值是 1 MiB，配置里写 `"1MB"` 会比默认值**小** 4.8%。
        let config: crate::config::AppConfig = ::config::Config::builder()
            .add_source(::config::File::from_str(
                "[template]\nmax_template_bytes = \"2MiB\"\n",
                ::config::FileFormat::Toml,
            ))
            .build()
            .and_then(::config::Config::try_deserialize)
            .expect("应能解析 \"2MiB\"");
        assert_eq!(config.template.max_template_bytes.as_u64(), 2 * 1024 * 1024);
    }
}
