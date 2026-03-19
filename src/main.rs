use ds_api::{McpServer, ToolBundle, tool};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ── constants ─────────────────────────────────────────────────────────────────

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const OUTPUT_LIMIT: usize = 8_000;

// ── cargo root ────────────────────────────────────────────────────────────────

fn find_cargo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        if cur.join("Cargo.toml").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

// ── PATH helper ───────────────────────────────────────────────────────────────

fn cargo_path_env() -> String {
    #[cfg(windows)]
    let (home_var, cargo_suffix, sep) = ("USERPROFILE", r".cargo\bin", ";");
    #[cfg(not(windows))]
    let (home_var, cargo_suffix, sep) = ("HOME", ".cargo/bin", ":");

    #[cfg(windows)]
    let path_var = "Path";
    #[cfg(not(windows))]
    let path_var = "PATH";

    let current = std::env::var(path_var)
        .or_else(|_| std::env::var("PATH"))
        .unwrap_or_default();
    let cargo_bin = std::env::var(home_var)
        .map(|h| format!("{h}{}{cargo_suffix}", std::path::MAIN_SEPARATOR))
        .unwrap_or_default();
    if cargo_bin.is_empty() || current.contains(&cargo_bin) {
        current
    } else {
        format!("{cargo_bin}{sep}{current}")
    }
}

// ── output truncation ─────────────────────────────────────────────────────────

fn truncate_output(s: String) -> Value {
    let total = s.len();
    if total <= OUTPUT_LIMIT {
        return json!({ "output": s, "truncated": false });
    }
    // 截断到 char boundary
    let mut cut = OUTPUT_LIMIT;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    json!({
        "output": &s[..cut],
        "truncated": true,
        "total_bytes": total,
        "shown_bytes": cut,
    })
}

// ── bash execution ────────────────────────────────────────────────────────────

fn run_bash(command: &str, timeout_ms: u64) -> Value {
    use std::time::{Duration, Instant};

    #[cfg(windows)]
    let (prog, args) = ("cmd", vec!["/C", command]);
    #[cfg(not(windows))]
    let (prog, args) = ("sh", vec!["-c", command]);

    let path_env = cargo_path_env();

    let mut child = match Command::new(prog)
        .args(&args)
        .env("PATH", &path_env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return json!({ "error": format!("spawn failed: {e}") }),
    };

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);

    // 在单独线程里等待，主线程 sleep-poll 以支持超时

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // 已退出，收集输出
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = std::io::Read::read_to_end(&mut o, &mut stdout);
                }
                if let Some(mut e) = child.stderr.take() {
                    let _ = std::io::Read::read_to_end(&mut e, &mut stderr);
                }
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr),
                );
                let mut r = truncate_output(combined);
                r["exit_code"] = json!(status.code());
                r["timed_out"] = json!(false);
                break r;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    break json!({
                        "error": format!("timed out after {timeout_ms}ms"),
                        "timed_out": true,
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => break json!({ "error": format!("wait failed: {e}") }),
        }
    }
}

// ── diagnostic parsing ────────────────────────────────────────────────────────

fn parse_diagnostics(stderr: &str, crate_root: &Path) -> Vec<Value> {
    let mut diags: Vec<Value> = Vec::new();
    let lines: Vec<&str> = stderr.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let level = if line.starts_with("error") {
            "error"
        } else if line.starts_with("warning") {
            "warning"
        } else {
            i += 1;
            continue;
        };
        if line.contains("aborting due to") || line.contains("could not compile") {
            i += 1;
            continue;
        }
        let message = line
            .split_once(": ")
            .map(|x| x.1)
            .unwrap_or(line)
            .trim()
            .to_string();

        // 找 --> 位置行
        let mut location: Option<(String, usize, usize)> = None;
        let mut j = i + 1;
        while j < lines.len() && j < i + 6 {
            let loc = lines[j].trim();
            if let Some(rest) = loc.strip_prefix("--> ") {
                let p: Vec<&str> = rest.splitn(3, ':').collect();
                if p.len() >= 2
                    && let Ok(row) = p[1].parse::<usize>()
                {
                    let col = p.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
                    location = Some((p[0].to_string(), row, col));
                }
                break;
            }
            if lines[j].starts_with("error") || lines[j].starts_with("warning") {
                break;
            }
            j += 1;
        }

        // 收集原始诊断块
        let mut raw_lines = vec![line];
        let mut k = i + 1;
        while k < lines.len() {
            let next = lines[k];
            let is_new = !next.starts_with(' ')
                && !next.starts_with('\t')
                && (next.starts_with("error") || next.starts_with("warning"))
                && !next.trim().is_empty();
            if is_new {
                break;
            }
            raw_lines.push(next);
            k += 1;
        }

        // 读源码上下文
        let source_context = location.as_ref().and_then(|(rel, row, _)| {
            let abs = if Path::new(rel).is_absolute() {
                PathBuf::from(rel)
            } else {
                crate_root.join(rel)
            };
            let src = std::fs::read_to_string(&abs).ok()?;
            let src_lines: Vec<&str> = src.lines().collect();
            let total = src_lines.len();
            let center = row.saturating_sub(1);
            let start = center.saturating_sub(5);
            let end = (center + 6).min(total);
            let snippet: Vec<String> = src_lines[start..end]
                .iter()
                .enumerate()
                .map(|(idx, l)| {
                    let lineno = start + idx + 1;
                    let marker = if lineno == *row { ">>>" } else { "   " };
                    format!("{marker} {lineno:4} | {l}")
                })
                .collect();
            Some(json!({ "file": rel, "line": row, "snippet": snippet.join("\n") }))
        });

        let mut diag = json!({ "level": level, "message": message, "raw": raw_lines.join("\n") });
        if let Some((f, r, c)) = &location {
            diag["file"] = json!(f);
            diag["line"] = json!(r);
            diag["col"] = json!(c);
        }
        if let Some(ctx) = source_context {
            diag["source_context"] = ctx;
        }
        diags.push(diag);
        i = k;
    }
    diags
}

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

fn run_executable(path: &str) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    run_bash(path, DEFAULT_TIMEOUT_MS)
}

// ── autocheck ─────────────────────────────────────────────────────────────────

fn run_check_in_root(root: &Path) -> Value {
    let path_env = cargo_path_env();

    let fix = Command::new("cargo")
        .args(["clippy", "--fix", "--allow-dirty"])
        .current_dir(root)
        .env("PATH", &path_env)
        .output();
    let (clippy_ok, _clippy_stderr) = match fix {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (false, format!("spawn failed: {e}")),
    };

    let check = Command::new("cargo")
        .args(["check", "--message-format=human"])
        .current_dir(root)
        .env("PATH", &path_env)
        .output();
    let (check_ok, check_stderr) = match check {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (false, format!("spawn failed: {e}")),
    };

    let diags = parse_diagnostics(&check_stderr, root);
    let errors: Vec<&Value> = diags.iter().filter(|d| d["level"] == "error").collect();
    let warnings: Vec<&Value> = diags.iter().filter(|d| d["level"] == "warning").collect();

    json!({
        "success": check_ok,
        "clippy_fix_ok": clippy_ok,
        "summary": if check_ok {
            format!("✅ cargo check passed ({} warning(s))", warnings.len())
        } else {
            format!("❌ cargo check failed: {} error(s), {} warning(s)", errors.len(), warnings.len())
        },
        "errors": errors,
        "warnings": warnings,
    })
}

fn auto_check(path: &str) -> Value {
    let p = Path::new(path);
    let needs_check = p.extension().is_some_and(|e| e == "rs")
        || p.file_name().is_some_and(|n| n == "Cargo.toml");
    if !needs_check {
        return json!({ "success": null, "summary": "skipped: not a .rs or Cargo.toml file" });
    }
    let Some(root) = find_cargo_root(p) else {
        return json!({ "success": null, "summary": format!("skipped: no Cargo.toml above {path}") });
    };
    run_check_in_root(&root)
}

// ── tools ─────────────────────────────────────────────────────────────────────

/// 실제 파일 쓰기 로직 (autocheck 없이). write/multiwrite 양쪽에서 호출.
fn do_write(
    path: &str,
    new_content: String,
    old_string: Option<String>,
    count: Option<usize>,
    append: Option<bool>,
    shebang: Option<String>,
) -> Value {
    // ── shebang ───────────────────────────────────────────────────────────────
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

    // ── append mode ───────────────────────────────────────────────────────────
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
            let run = shebang.as_ref().map(|_| run_executable(path));
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
        let run = shebang.as_ref().map(|_| run_executable(path));
        return json!({ "appended": path, "bytes": new_content.len(), "diff": diff, "run": run });
    }

    // ── replace mode ──────────────────────────────────────────────────────────
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
        let run = shebang.as_ref().map(|_| run_executable(path));
        return json!({ "replaced": path, "occurrences": found, "diff": diff, "run": run });
    }

    // ── overwrite mode ────────────────────────────────────────────────────────
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
    let run = shebang.as_ref().map(|_| run_executable(path));
    json!({ "written": path, "bytes": new_content.len(), "diff": diff, "run": run })
}

struct Tools;

#[tool]
impl ds_api::Tool for Tools {
    /// Write multiple files in one call, then run cargo check once at the end.
    /// writes_json: JSON array string. Each element has the same fields as the `write` tool:
    ///   path (required), new_content (required), old_string, count, append, shebang
    async fn multiwrite(&self, writes_json: String) -> Value {
        let arr: Vec<Value> = match serde_json::from_str(&writes_json) {
            Ok(Value::Array(a)) => a,
            _ => return json!({ "error": "writes_json must be a JSON array" }),
        };

        let mut results = Vec::new();
        let mut cargo_root: Option<PathBuf> = None;

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

            // 找 cargo root（取第一个 .rs 文件的）
            if cargo_root.is_none() {
                cargo_root = find_cargo_root(Path::new(&path));
            }

            // 执行写操作（跳过 autocheck，自己做）
            let write_result = do_write(&path, new_content, old_string, count, append, shebang);
            results.push(write_result);
        }

        // 统一跑一次 cargo check
        let autocheck = match cargo_root {
            Some(root) => run_check_in_root(&root),
            None => json!({ "success": null, "summary": "skipped: no Cargo.toml found" }),
        };

        json!({ "results": results, "autocheck": autocheck })
    }

    /// Execute a shell command and return combined stdout+stderr (truncated to 8000 chars).
    /// command: the shell command to run
    /// timeout_ms: max milliseconds to wait (default 10000)
    async fn bash(&self, command: String, timeout_ms: Option<u64>) -> Value {
        run_bash(&command, timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }

    /// Write to a file (overwrite / replace / append), then run cargo clippy --fix + cargo check.
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
        let mut result = do_write(&path, new_content, old_string, count, append, shebang);
        result["autocheck"] = auto_check(&path);
        result
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    McpServer::new(ToolBundle::new().add(Tools))
        .with_name("autocheck-mcp")
        .serve_stdio()
        .await?;
    Ok(())
}
