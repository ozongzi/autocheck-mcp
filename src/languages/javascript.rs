use async_trait::async_trait;
use std::path::Path;
use tokio::process::Command;
use crate::languages::{LanguageSupport, CheckResult};
use crate::diagnostics::parse_generic_diagnostics;

pub struct JavaScriptSupport;

#[async_trait]
impl LanguageSupport for JavaScriptSupport {
    fn root_markers(&self) -> &'static [&'static str] {
        &["package.json", "tsconfig.json", ".git"]
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["js", "ts", "jsx", "tsx"]
    }
    async fn run_check(&self, root: &Path, _file_path: Option<&Path>) -> CheckResult {
        // Try npx eslint
        let eslint_check = Command::new("npx")
            .args(["eslint", "--version"])
            .current_dir(root)
            .status()
            .await
            .is_ok_and(|s| s.success());

        let mut fix_ok = false;
        let mut check_stderr = String::new();
        let mut check_stdout = String::new();
        let mut check_ok = true;

        if eslint_check {
            fix_ok = Command::new("npx")
                .args(["eslint", "--fix", "."])
                .current_dir(root)
                .status()
                .await
                .is_ok_and(|s| s.success());

            let check = Command::new("npx")
                .args(["eslint", "."])
                .current_dir(root)
                .output()
                .await;

            if let Ok(o) = check {
                check_ok = o.status.success();
                check_stdout = String::from_utf8_lossy(&o.stdout).to_string();
                check_stderr = String::from_utf8_lossy(&o.stderr).to_string();
            }
        }

        // Try npx tsc if tsconfig exists
        if root.join("tsconfig.json").exists() {
            let tsc = Command::new("npx")
                .args(["tsc", "--noEmit"])
                .current_dir(root)
                .output()
                .await;
            
            if let Ok(o) = tsc {
                check_ok = check_ok && o.status.success();
                check_stdout.push_str(&String::from_utf8_lossy(&o.stdout));
                check_stderr.push_str(&String::from_utf8_lossy(&o.stderr));
            }
        }

        let combined = format!("{}{}", check_stdout, check_stderr);
        let diags = parse_generic_diagnostics(&combined, root);
        let errors: Vec<_> = diags.iter().filter(|d| d["level"] == "error").cloned().collect();
        let warnings: Vec<_> = diags.iter().filter(|d| d["level"] == "warning").cloned().collect();

        CheckResult {
            success: check_ok,
            fix_ok,
            summary: if check_ok {
                format!("✅ JS/TS check passed ({} warning(s))", warnings.len())
            } else {
                format!("❌ JS/TS check failed: {} error(s), {} warning(s)", errors.len(), warnings.len())
            },
            errors,
            warnings,
        }
    }
}
