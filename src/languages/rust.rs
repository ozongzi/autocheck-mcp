use crate::diagnostics::parse_rust_diagnostics;
use crate::languages::{CheckResult, LanguageSupport};
use crate::utils::path_env_with_cargo;
use async_trait::async_trait;
use std::path::Path;
use tokio::process::Command;

pub struct RustSupport;

#[async_trait]
impl LanguageSupport for RustSupport {
    fn root_markers(&self) -> &'static [&'static str] {
        &["Cargo.toml"]
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }
    async fn run_check(&self, root: &Path, _file_path: Option<&Path>) -> CheckResult {
        let path_env = path_env_with_cargo();

        let fix = Command::new("cargo")
            .args(["clippy", "--fix", "--allow-dirty"])
            .current_dir(root)
            .env("PATH", &path_env)
            .output()
            .await;
        let fix_ok = fix.is_ok_and(|o| o.status.success());

        let check = Command::new("cargo")
            .args(["clippy", "--message-format=human"])
            .current_dir(root)
            .env("PATH", &path_env)
            .output()
            .await;

        let (check_ok, check_stderr) = match check {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stderr).to_string(),
            ),
            Err(e) => (false, format!("spawn failed: {e}")),
        };

        let diags = parse_rust_diagnostics(&check_stderr, root);
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d["level"] == "error")
            .cloned()
            .collect();
        let warnings: Vec<_> = diags
            .iter()
            .filter(|d| d["level"] == "warning")
            .cloned()
            .collect();

        CheckResult {
            success: check_ok,
            fix_ok,
            summary: if check_ok {
                format!(
                    "✅ cargo clippy (include check) passed ({} warning(s))",
                    warnings.len()
                )
            } else {
                format!(
                    "❌ cargo clippy (include check) failed: {} error(s), {} warning(s)",
                    errors.len(),
                    warnings.len()
                )
            },
            errors,
            warnings,
        }
    }
}
