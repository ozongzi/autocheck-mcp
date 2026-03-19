use async_trait::async_trait;
use std::path::Path;
use tokio::process::Command;
use crate::languages::{LanguageSupport, CheckResult};
use crate::diagnostics::parse_generic_diagnostics;

pub struct GoSupport;

#[async_trait]
impl LanguageSupport for GoSupport {
    fn root_markers(&self) -> &'static [&'static str] {
        &["go.mod"]
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }
    async fn run_check(&self, root: &Path, file_path: Option<&Path>) -> CheckResult {
        // Run go fmt on the file or root
        let fix_ok = if let Some(p) = file_path {
            Command::new("go")
                .args(["fmt", p.to_str().unwrap_or(".")])
                .current_dir(root)
                .status()
                .await
                .is_ok_and(|s| s.success())
        } else {
            Command::new("go")
                .args(["fmt", "./..."])
                .current_dir(root)
                .status()
                .await
                .is_ok_and(|s| s.success())
        };

        // Run go vet
        let check = Command::new("go")
            .args(["vet", "./..."])
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
        let warnings: Vec<_> = diags.iter().filter(|d| d["level"] == "warning").cloned().collect();

        CheckResult {
            success: check_ok,
            fix_ok,
            summary: if check_ok {
                format!("✅ go vet passed ({} warning(s))", warnings.len())
            } else {
                format!("❌ go vet failed: {} error(s), {} warning(s)", errors.len(), warnings.len())
            },
            errors,
            warnings,
        }
    }
}
