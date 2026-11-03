//! 模板来源（[`TemplateSource`]）。
//!
//! 框架**不假定**模板存在哪：文件目录（[`FsSource`]）、内存（[`MemorySource`]）、
//! 数据库 / 对象存储（业务自己实现 trait）都走同一个接口，一次 `load` 返回
//! 「模板名 → 源码」的全量快照。
//!
//! 全量快照而不是增量通知，是为了让重载具备**原子性**：要么整份新环境生效，
//! 要么保持旧环境（见 [`TemplateEngine`](crate::template::TemplateEngine)）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::template::error::TemplateError;

/// 模板来源：返回「模板名 → 源码」的全量快照。
///
/// 实现方可以做阻塞 I/O（读目录 / 查库）：`load` 只在启动期与显式重载时调用。
pub trait TemplateSource: Send + Sync {
    /// 拉取全量模板。
    ///
    /// # Errors
    /// 目录不可读、单模板超限、模板名非法等，返回
    /// [`TemplateError::load`](TemplateError::load)。
    fn load(&self) -> Result<Vec<(String, String)>, TemplateError>;
}

/// 校验模板名。
///
/// 模板名统一用 `/` 分隔；**拒绝**绝对路径、`\`、驱动器前缀（`:`）、空段、
/// `.` 与 `..`——这些是路径穿越的全部入口。
///
/// # Errors
/// 名字非法时返回 [`TemplateError`]（类别为
/// [`Source`](crate::template::TemplateErrorKind::Source)）。
pub fn validate_name(name: &str) -> Result<(), TemplateError> {
    if name.trim().is_empty() {
        return Err(TemplateError::load("模板名不能为空"));
    }
    if name.starts_with('/') || name.contains('\\') || name.contains(':') || name.contains('\0') {
        return Err(TemplateError::load(format!(
            "非法模板名 \"{name}\"：不允许绝对路径、反斜杠、驱动器前缀或 NUL"
        )));
    }
    if name
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(TemplateError::load(format!(
            "非法模板名 \"{name}\"：不允许空段、`.` 或 `..`"
        )));
    }
    Ok(())
}

/// 文件系统模板来源：递归扫描目录，模板名 = `/` 分隔的相对路径。
#[derive(Debug, Clone)]
pub struct FsSource {
    dir: PathBuf,
    extensions: Vec<String>,
    max_template_bytes: u64,
}

impl FsSource {
    /// 以默认扩展名（`html` / `htm` / `jinja` / `jinja2`）与 1 MiB 单模板上限构造。
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            extensions: vec![
                "html".to_string(),
                "htm".to_string(),
                "jinja".to_string(),
                "jinja2".to_string(),
            ],
            max_template_bytes: 1024 * 1024,
        }
    }

    /// 设置扩展名白名单（自动去掉前导 `.` 并转小写）。
    #[must_use]
    pub fn with_extensions<I, S>(mut self, extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extensions = extensions
            .into_iter()
            .map(|extension| {
                extension
                    .into()
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase()
            })
            .filter(|extension| !extension.is_empty())
            .collect();
        self
    }

    /// 设置单个模板的体积上限（字节）。
    #[must_use]
    pub fn with_max_template_bytes(mut self, bytes: u64) -> Self {
        self.max_template_bytes = bytes;
        self
    }

    /// 模板根目录。
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn accepts(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| {
                let extension = extension.to_ascii_lowercase();
                self.extensions.contains(&extension)
            })
            .unwrap_or(false)
    }
}

impl TemplateSource for FsSource {
    fn load(&self) -> Result<Vec<(String, String)>, TemplateError> {
        // 目录不存在 / 不可读 → 直接失败（fail fast，docs/20 §12-2）。
        // 「空目录」才是合法的「暂无模板」，两者不能混为一谈。
        let root = self.dir.canonicalize().map_err(|error| {
            TemplateError::load(format!("模板目录不可读（{}）：{error}", self.dir.display()))
        })?;

        let mut templates: Vec<(String, String)> = Vec::new();
        let mut stack = vec![root.clone()];

        while let Some(current) = stack.pop() {
            let entries = std::fs::read_dir(&current).map_err(|error| {
                TemplateError::load(format!("读取目录失败（{}）：{error}", current.display()))
            })?;

            for entry in entries {
                let entry = entry.map_err(|error| {
                    TemplateError::load(format!("遍历目录失败（{}）：{error}", current.display()))
                })?;
                let file_type = entry.file_type().map_err(|error| {
                    TemplateError::load(format!(
                        "读取文件类型失败（{}）：{error}",
                        entry.path().display()
                    ))
                })?;

                // 隐藏文件 / 编辑器临时文件（`.foo.html.swp`）一律跳过：
                // 它们不是模板，混进来只会让「改了没生效」变成玄学。
                let file_name = entry.file_name().to_string_lossy().into_owned();
                if file_name.starts_with('.') {
                    continue;
                }

                if file_type.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                // 符号链接既不是 `is_file` 也不是 `is_dir`，这里自然跳过：
                // 链接可以指向根目录之外，跟随它就等于开了个路径穿越口子。
                if !file_type.is_file() || !self.accepts(&entry.path()) {
                    continue;
                }

                let path = entry.path();
                let relative = path.strip_prefix(&root).map_err(|error| {
                    TemplateError::load(format!(
                        "计算模板相对路径失败（{}）：{error}",
                        path.display()
                    ))
                })?;
                let name = relative.to_string_lossy().replace('\\', "/");
                validate_name(&name)?;

                let metadata = entry.metadata().map_err(|error| {
                    TemplateError::load(format!(
                        "读取文件元信息失败（{}）：{error}",
                        path.display()
                    ))
                })?;
                if metadata.len() > self.max_template_bytes {
                    return Err(TemplateError::load(format!(
                        "模板 \"{name}\" 体积 {} 字节，超过上限 {} 字节\
                         （[template].max_template_bytes）",
                        metadata.len(),
                        self.max_template_bytes
                    )));
                }

                let source = std::fs::read_to_string(&path).map_err(|error| {
                    TemplateError::load(format!(
                        "读取模板 \"{name}\" 失败（仅支持 UTF-8）：{error}"
                    ))
                })?;
                templates.push((name, source));
            }
        }

        // 名字有序：`names()` 的输出与错误信息里的先后都稳定，便于测试与排障。
        templates.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(templates)
    }
}

/// 内存模板来源：测试、代码内注册、以及「业务已从 DB 取好全量模板」的场景。
#[derive(Debug, Clone, Default)]
pub struct MemorySource {
    templates: BTreeMap<String, String>,
}

impl MemorySource {
    /// 空集合。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入一个模板（同名覆盖）。
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.templates.insert(name.into(), source.into());
        self
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// 模板数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.templates.len()
    }
}

impl TemplateSource for MemorySource {
    fn load(&self) -> Result<Vec<(String, String)>, TemplateError> {
        let mut templates = Vec::with_capacity(self.templates.len());
        for (name, source) in &self.templates {
            validate_name(name)?;
            templates.push((name.clone(), source.clone()));
        }
        Ok(templates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时目录守卫：测试结束（含 panic）时清理。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aurora-template-{tag}-{}",
                crate::utils::Id::uuid()
            ));
            std::fs::create_dir_all(&dir).expect("创建临时目录");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("创建父目录");
            }
            std::fs::write(path, content).expect("写入文件");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn should_reject_path_traversal_names() {
        for name in [
            "../etc/passwd",
            "admin/../../secret.html",
            "/etc/passwd",
            "c:/windows/win.ini",
            "admin\\user.html",
            "admin//user.html",
            "./index.html",
            "",
            "   ",
        ] {
            assert!(
                validate_name(name).is_err(),
                "\"{name}\" 必须被拒绝（路径穿越入口）"
            );
        }
        for name in ["index.html", "admin/users.html", "邮件/欢迎.html"] {
            validate_name(name).expect("正常名字应通过");
        }
    }

    #[test]
    fn should_scan_dir_recursively_and_filter_extension() {
        let dir = TempDir::new("fs-scan");
        dir.write("index.html", "index");
        dir.write("admin/users.html", "users");
        dir.write("notes.txt", "忽略：扩展名不在白名单");
        dir.write(".hidden.html", "忽略：隐藏文件");

        let loaded = FsSource::new(dir.path()).load().expect("扫描应成功");
        let names: Vec<&str> = loaded.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["admin/users.html", "index.html"],
            "递归扫描、`/` 分隔、按名排序，且过滤扩展名与隐藏文件"
        );
    }

    #[test]
    fn should_fail_when_dir_missing() {
        let error = FsSource::new(std::env::temp_dir().join("aurora-template-not-exists-xyz"))
            .load()
            .expect_err("目录不存在必须失败（fail fast）");
        assert!(
            error.to_string().contains("模板目录不可读"),
            "错误信息应指出目录不可读：{error}"
        );
    }

    #[test]
    fn should_accept_empty_dir() {
        let dir = TempDir::new("fs-empty");
        let loaded = FsSource::new(dir.path()).load().expect("空目录是合法的");
        assert!(loaded.is_empty(), "空目录 = 暂无模板，不是错误");
    }

    #[test]
    fn should_reject_oversized_template() {
        let dir = TempDir::new("fs-oversize");
        dir.write("big.html", "0123456789");

        let error = FsSource::new(dir.path())
            .with_max_template_bytes(4)
            .load()
            .expect_err("超过体积上限必须失败");
        assert!(error.to_string().contains("超过上限"), "实际：{error}");
    }

    #[test]
    fn should_validate_names_from_memory_source() {
        let ok = MemorySource::new().with("index.html", "hi");
        assert_eq!(ok.len(), 1);
        assert_eq!(ok.load().expect("合法").len(), 1);

        let bad = MemorySource::new().with("../escape.html", "hi");
        assert!(bad.load().is_err(), "内存来源同样要过名字校验");
    }
}
