use ds_api::{McpServer, ToolBundle, tool};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use autocheck_mcp::languages::{Language, detect_language, get_support};
use autocheck_mcp::utils::{DEFAULT_TIMEOUT_MS, find_root, run_bash};

// ── file context snippet ──────────────────────────────────────────────────────

fn make_diff(path: &str, before: &str, after: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(before, after);
    let result = diff
        .unified_diff()
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    if result.is_empty() {
        "(no changes)".to_string()
    } else {
        result
    }
}

// ── run executable ────────────────────────────────────────────────────────────

async fn run_executable(path: &str) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    run_bash(path, DEFAULT_TIMEOUT_MS).await
}

// ── autocheck ─────────────────────────────────────────────────────────────────

async fn auto_check(path: &str) -> Value {
    let p = Path::new(path);
    let Some(lang) = detect_language(p) else {
        return json!({ "success": null, "summary": format!("skipped: unsupported file type for {path}") });
    };
    let support = get_support(lang);
    let Some(root) = find_root(p, support.root_markers()) else {
        return json!({ "success": null, "summary": format!("skipped: no root markers found above {path}") });
    };
    support.run_check(&root, Some(p)).await.to_json()
}

// ── tools implementation ──────────────────────────────────────────────────────

async fn do_write(
    path: &str,
    new_content: String,
    old_string: Option<String>,
    count: Option<usize>,
    append: Option<bool>,
    shebang: Option<String>,
) -> Value {
    let new_content = if let Some(ref shebang) = shebang {
        let line = if shebang.starts_with("#!") {
            shebang.clone()
        } else {
            format!("#!{shebang}")
        };
        format!("{line}\n{new_content}")
    } else {
        new_content
    };

    if append.unwrap_or(false) {
        if let Some(ref anchor) = old_string {
            let original = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => return json!({ "error": format!("read failed: {e}") }),
            };
            if !original.contains(anchor.as_str()) {
                return json!({ "error": "old_string not found" });
            }
            let updated = original.replacen(anchor.as_str(), &format!("{anchor}{new_content}"), 1);
            if let Err(e) = std::fs::write(path, &updated) {
                return json!({ "error": format!("write failed: {e}") });
            }
            let diff = make_diff(path, &original, &updated);
            let run = if shebang.is_some() {
                Some(run_executable(path).await)
            } else {
                None
            };
            return json!({ "inserted_after": path, "bytes": new_content.len(), "diff": diff, "run": run });
        }
        use std::fs::OpenOptions;
        let before = std::fs::read_to_string(path).unwrap_or_default();
        match OpenOptions::new().create(true).append(true).open(path) {
            Err(e) => return json!({ "error": format!("open failed: {e}") }),
            Ok(mut f) => {
                if let Err(e) = f.write_all(new_content.as_bytes()) {
                    return json!({ "error": format!("write failed: {e}") });
                }
            }
        }
        let after = std::fs::read_to_string(path).unwrap_or_default();
        let diff = make_diff(path, &before, &after);
        let run = if shebang.is_some() {
            Some(run_executable(path).await)
        } else {
            None
        };
        return json!({ "appended": path, "bytes": new_content.len(), "diff": diff, "run": run });
    }

    if let Some(old) = old_string {
        let original = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return json!({ "error": format!("read failed: {e}") }),
        };
        let found = original.matches(old.as_str()).count();
        if found == 0 {
            return json!({ "error": "old_string not found" });
        }
        let expected = count.unwrap_or(1);
        if expected != 0 && found != expected {
            return json!({ "error": format!("expected {expected} occurrence(s) but found {found}") });
        }
        let updated = if expected == 0 {
            original.replace(old.as_str(), &new_content)
        } else {
            let mut s = original.clone();
            for _ in 0..expected {
                s = s.replacen(old.as_str(), &new_content, 1);
            }
            s
        };
        if let Err(e) = std::fs::write(path, &updated) {
            return json!({ "error": format!("write failed: {e}") });
        }
        let diff = make_diff(path, &original, &updated);
        let run = if shebang.is_some() {
            Some(run_executable(path).await)
        } else {
            None
        };
        return json!({ "replaced": path, "occurrences": found, "diff": diff, "run": run });
    }

    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return json!({ "error": format!("mkdir failed: {e}") });
    }
    let before = std::fs::read_to_string(path).unwrap_or_default();
    if let Err(e) = std::fs::write(path, &new_content) {
        return json!({ "error": format!("write failed: {e}") });
    }
    let diff = make_diff(path, &before, &new_content);
    let run = if shebang.is_some() {
        Some(run_executable(path).await)
    } else {
        None
    };
    json!({ "written": path, "bytes": new_content.len(), "diff": diff, "run": run })
}

struct Tools;

#[tool]
impl ds_api::Tool for Tools {
    /// Write multiple files in one call, then run checks (autocheck) for all affected projects once at the end.
    /// writes_json: JSON array string. Each element has the same fields as the `write` tool:
    ///   path (required), new_content (required), old_string, count, append, shebang
    async fn multiwrite(&self, writes_json: String) -> Value {
        let arr: Vec<Value> = match serde_json::from_str(&writes_json) {
            Ok(Value::Array(a)) => a,
            _ => return json!({ "error": "writes_json must be a JSON array" }),
        };

        let mut results = Vec::new();
        let mut affected: HashSet<(PathBuf, Language)> = HashSet::new();

        for item in &arr {
            let path = match item["path"].as_str() {
                Some(p) => p.to_string(),
                None => {
                    results.push(json!({ "error": "missing path" }));
                    continue;
                }
            };
            let new_content = item["new_content"].as_str().unwrap_or("").to_string();
            let old_string = item["old_string"].as_str().map(str::to_string);
            let count = item["count"].as_u64().map(|n| n as usize);
            let append = item["append"].as_bool();
            let shebang = item["shebang"].as_str().map(str::to_string);

            let write_result =
                do_write(&path, new_content, old_string, count, append, shebang).await;
            results.push(write_result);

            let p = Path::new(&path);
            if let Some(lang) = detect_language(p) {
                let support = get_support(lang);
                if let Some(root) = find_root(p, support.root_markers()) {
                    affected.insert((root, lang));
                }
            }
        }

        let mut autochecks = Vec::new();
        for (root, lang) in affected {
            let support = get_support(lang);
            autochecks.push(support.run_check(&root, None).await.to_json());
        }

        json!({ "results": results, "autochecks": autochecks })
    }

    async fn bash(&self, command: String, timeout_ms: Option<u64>) -> Value {
        run_bash(&command, timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)).await
    }

    /// Write to a file (overwrite / replace / append), then run language-specific check (autocheck).
    /// Can also used for normal text manipulation tasks, not necessarily code files.
    ///
    /// Modes:
    ///   - old_string omitted, append omitted → overwrite entire file with new_content (or create)
    ///   - old_string present, append omitted → replace occurrences of old_string with new_content
    ///   - append = true, old_string omitted  → append new_content to end of file
    ///   - append = true, old_string present  → insert new_content immediately after old_string
    ///
    /// path: absolute path to the file
    /// new_content: content to write, replacement text, or text to insert
    /// old_string: exact text to find and replace (or anchor for insert-after)
    /// count: expected number of replacements when using old_string in replace mode (default 1, 0 = replace all)
    /// append: if true, append to end of file or insert after old_string
    /// shebang: if provided, prepend this shebang line and execute the file after writing
    async fn write(
        &self,
        path: String,
        new_content: String,
        old_string: Option<String>,
        count: Option<usize>,
        append: Option<bool>,
        shebang: Option<String>,
    ) -> Value {
        let mut result = do_write(&path, new_content, old_string, count, append, shebang).await;
        result["autocheck"] = auto_check(&path).await;
        result
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    McpServer::new(ToolBundle::new().add(Tools))
        .with_name("autocheck-mcp")
        .serve_stdio()
        .await?;
    Ok(())
}
