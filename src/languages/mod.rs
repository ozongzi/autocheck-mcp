use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;

pub mod rust;
pub mod go;
pub mod python;
pub mod javascript;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Go,
    Python,
    JavaScript,
}

#[derive(Debug)]
pub struct CheckResult {
    pub success: bool,
    pub fix_ok: bool,
    pub summary: String,
    pub errors: Vec<Value>,
    pub warnings: Vec<Value>,
}

impl CheckResult {
    pub fn to_json(&self) -> Value {
        json!({
            "success": self.success,
            "fix_ok": self.fix_ok,
            "summary": self.summary,
            "errors": self.errors,
            "warnings": self.warnings,
        })
    }
}

#[async_trait]
pub trait LanguageSupport: Send + Sync {
    fn root_markers(&self) -> &'static [&'static str];
    fn extensions(&self) -> &'static [&'static str];
    async fn run_check(&self, root: &Path, file_path: Option<&Path>) -> CheckResult;
}

pub fn detect_language(path: &Path) -> Option<Language> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some(Language::Rust),
        "go" => Some(Language::Go),
        "py" => Some(Language::Python),
        "js" | "ts" | "jsx" | "tsx" => Some(Language::JavaScript),
        _ => {
            if path.file_name().is_some_and(|n| n == "Cargo.toml") {
                Some(Language::Rust)
            } else if path.file_name().is_some_and(|n| n == "go.mod") {
                Some(Language::Go)
            } else if path.file_name().is_some_and(|n| n == "package.json") {
                Some(Language::JavaScript)
            } else {
                None
            }
        }
    }
}

pub fn get_support(lang: Language) -> Box<dyn LanguageSupport> {
    match lang {
        Language::Rust => Box::new(rust::RustSupport),
        Language::Go => Box::new(go::GoSupport),
        Language::Python => Box::new(python::PythonSupport),
        Language::JavaScript => Box::new(javascript::JavaScriptSupport),
    }
}
