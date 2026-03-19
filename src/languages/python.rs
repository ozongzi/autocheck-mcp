use async_trait::async_trait;
use std::path::Path;
use tokio::process::Command;
use crate::languages::{LanguageSupport, CheckResult};
use crate::diagnostics::parse_generic_diagnostics;

pub struct PythonSupport;

#[async_trait]
impl LanguageSupport for PythonSupport {
    fn root_markers(&self) -> &'static [&'static str] {
        &["pyproject.toml", "requirements.txt", "setup.py", ".git"]
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["py"]
    }
    async fn run_check(&self, root: &Path, file_path: Option<&Path>) -> CheckResult {
        // Try ruff
        let ruff_check = Command::new("ruff")
            .arg("--version")
            .status()
            .await
            .is_ok_and(|s| s.success());

        if ruff_check {
            let fix_ok = Command::new("ruff")
                .args(["check", "--fix", "."])
                .current_dir(root)
                .status()
                .await
                .is_ok_and(|s| s.success());

            let check = Command::new("ruff")
                .args(["check", "."])
                .current_dir(root)
                .output()
                .await;

            let (check_ok, check_stdout, check_stderr) = match check {
                Ok(o) => (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stdout).to_string(),
                    String::from_utf8_lossy(&o.stderr).to_string(),
                ),
                Err(e) => (false, String::new(), format!("spawn failed: {e}")),
            };

            let combined = format!("{}{}", check_stdout, check_stderr);
            let diags = parse_generic_diagnostics(&combined, root);
            let errors: Vec<_> = diags.iter().filter(|d| d["level"] == "error").cloned().collect();
            let warnings: Vec<_> = diags.iter().filter(|d| d["level"] == "warning").cloned().collect();

            return CheckResult {
                success: check_ok,
                fix_ok,
                summary: if check_ok {
                    format!("✅ ruff check passed ({} warning(s))", warnings.len())
                } else {
                    format!("❌ ruff check failed: {} error(s), {} warning(s)", errors.len(), warnings.len())
                },
                errors,
                warnings,
            };
        }

        // Fallback to py_compile
        if let Some(p) = file_path {
            let check = Command::new("python3")
                .args(["-m", "py_compile", p.to_str().unwrap_or(".")])
                .current_dir(root)
                .output()
                .await;

            let (check_ok, check_stderr) = match check {
                Ok(o) => (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stderr).to_string(),
                ),
                Err(e) => (false, format!("spawn failed: {e}")),
            };

            let diags = parse_generic_diagnostics(&check_stderr, root);
            let errors: Vec<_> = diags.iter().filter(|d| d["level"] == "error").cloned().collect();

            CheckResult {
                success: check_ok,
                fix_ok: false,
                summary: if check_ok {
                    "✅ python compilation passed".to_string()
                } else {
                    format!("❌ python compilation failed: {} error(s)", errors.len())
                },
                errors,
                warnings: Vec::new(),
            }
        } else {
            CheckResult {
                success: true,
                fix_ok: false,
                summary: "✅ skipped python check (no file provided and ruff not found)".to_string(),
                errors: Vec::new(),
                warnings: Vec::new(),
            }
        }
    }
}
