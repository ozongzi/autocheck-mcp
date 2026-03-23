use ds_api::{McpServer, ToolBundle, tool};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use regex::Regex;

use autocheck_mcp::languages::{Language, detect_language, get_support};
use autocheck_mcp::utils::{DEFAULT_TIMEOUT_MS, OUTPUT_LIMIT, find_root, run_bash};

// ── file context snippet ──────────────────────────────────────────────────────

fn not_found_error(content: String) -> Value {
    let total = content.len();
    let mut cut = OUTPUT_LIMIT.min(total);
    while !content.is_char_boundary(cut) { cut -= 1; }
    json!({
        "error": "old_string not found",
        "file_content": &content[..cut],
        "truncated": total > cut,
        "total_bytes": total,
    })
}

// ── read tool helpers ─────────────────────────────────────────────────────────

const MAX_LINES: usize = 1000;
const MAX_DIR_DEPTH: usize = 5;
const MAX_DIR_ITEMS: usize = 50;

fn build_tree(dir: &Path, prefix: &str, depth: usize, max_depth: usize) -> String {
    if depth > max_depth {
        return format!("{}└── ... (max depth reached)\n", prefix);
    }

    let mut result = String::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return format!("{}└── [access denied]\n", prefix),
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let total = entries.len();
    let show_count = total.min(MAX_DIR_ITEMS);

    for (i, entry) in entries.iter().take(show_count).enumerate() {
        let is_last = i == show_count - 1 && total <= MAX_DIR_ITEMS;
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let is_dir = path.is_dir();

        let connector = if is_last { "└── " } else { "├── " };
        let item = if is_dir {
            format!("{}{}{}/\n", prefix, connector, name)
        } else {
            format!("{}{}{}\n", prefix, connector, name)
        };
        result.push_str(&item);

        if is_dir && depth < max_depth {
            let new_prefix = if is_last {
                format!("{}    ", prefix)
            } else {
                format!("{}│   ", prefix)
            };
            result.push_str(&build_tree(&path, &new_prefix, depth + 1, max_depth));
        }
    }

    if total > MAX_DIR_ITEMS {
        result.push_str(&format!("{}└── ... ({} more items)\n", prefix, total - MAX_DIR_ITEMS));
    }

    result
}

fn run_ctags(path: &Path) -> Option<String> {
    let output = std::process::Command::new("ctags")
        .args(["-f", "-", "--fields=n", "--sort=no", path.to_str()?])
        .output()
        .ok()?;
    
    if !output.status.success() {
        return None;
    }
    
    String::from_utf8(output.stdout).ok()
}

fn parse_ctags_output(ctags_output: &str) -> Vec<(String, String, usize)> {
    ctags_output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3 {
                let name = parts[0].to_string();
                let kind = parts.get(3).and_then(|s| s.strip_prefix("kind:")).unwrap_or("?").to_string();
                let line_num = parts.last()
                    .and_then(|s| s.strip_prefix("line:"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                Some((name, kind, line_num))
            } else {
                None
            }
        })
        .collect()
}

fn extract_outline_ctags(path: &Path) -> Option<String> {
    let ctags_output = run_ctags(path)?;
    let tags = parse_ctags_output(&ctags_output);
    
    let outline: Vec<String> = tags
        .into_iter()
        .map(|(name, kind, line)| format!("{:4} | [{}] {}", line, kind, name))
        .collect();
    
    Some(outline.join("\n"))
}

fn extract_symbol_ctags(path: &Path, symbol: &str) -> Option<String> {
    let ctags_output = run_ctags(path)?;
    let tags = parse_ctags_output(&ctags_output);
    
    let target = tags.iter().find(|(name, _, _)| name == symbol)?;
    let target_line = target.2;
    
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    
    // Find the next tag's line number as the end boundary
    let end_line = tags
        .iter()
        .filter(|(_, _, line)| *line > target_line)
        .map(|(_, _, line)| *line)
        .min()
        .unwrap_or(lines.len());
    
    let symbol_lines = lines.get(target_line - 1..end_line.saturating_sub(1))?
        .join("\n");
    
    Some(symbol_lines)
}

fn add_line_numbers(content: &str, start_line: usize) -> String {
    content.lines()
        .enumerate()
        .map(|(i, line)| format!("{:4} | {}", start_line + i, line))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn do_read(
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    search_regex: Option<String>,
    context_lines: Option<usize>,
    outline_only: Option<bool>,
    extract_symbol: Option<String>,
    max_depth: Option<usize>,
) -> Value {
    let p = Path::new(path);

    // Directory mode
    if p.is_dir() {
        let depth = max_depth.unwrap_or(3).min(MAX_DIR_DEPTH);
        let tree = build_tree(p, "", 0, depth);
        return json!({
            "type": "directory",
            "path": path,
            "tree": tree,
        });
    }

    // Check if file exists
    if !p.exists() {
        return json!({ "error": format!("file not found: {path}") });
    }

    // Read file content
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": format!("read failed: {e}") }),
    };

    // Outline mode (ctags)
    if outline_only.unwrap_or(false) {
        if let Some(outline) = extract_outline_ctags(p) {
            return json!({
                "type": "outline",
                "path": path,
                "outline": outline,
            });
        }
        return json!({
            "type": "outline",
            "path": path,
            "outline": "(ctags not available or no symbols found)",
        });
    }

    // Extract symbol mode (ctags)
    if let Some(symbol) = extract_symbol {
        if let Some(symbol_body) = extract_symbol_ctags(p, &symbol) {
            return json!({
                "type": "symbol",
                "path": path,
                "symbol": symbol,
                "content": add_line_numbers(&symbol_body, 1),
                "line_count": symbol_body.lines().count(),
            });
        }
        return json!({
            "error": format!("symbol '{}' not found", symbol),
        });
    }

    // Search regex mode
    if let Some(pattern) = search_regex {
        let ctx = context_lines.unwrap_or(2);
        let regex = match Regex::new(&pattern) {
            Ok(r) => r,
            Err(e) => return json!({ "error": format!("invalid regex: {e}") }),
        };

        let lines: Vec<&str> = content.lines().collect();
        let mut matches = Vec::new();
        let mut match_indices = HashSet::new();

        for (i, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                match_indices.insert(i);
                let start = i.saturating_sub(ctx);
                let end = (i + ctx + 1).min(lines.len());
                for j in start..end {
                    matches.push((j, lines[j], j == i));
                }
            }
        }

        if matches.is_empty() {
            return json!({ "matches": [], "total_matches": 0 });
        }

        // Remove duplicates and sort
        let mut seen = HashSet::new();
        let formatted: Vec<String> = matches
            .into_iter()
            .filter(|(idx, _, _)| seen.insert(*idx))
            .map(|(idx, line, is_match)| {
                let marker = if is_match { ">>>" } else { "   " };
                format!("{:4} | {} {}", idx + 1, marker, line)
            })
            .collect();

        return json!({
            "type": "search",
            "path": path,
            "pattern": pattern,
            "matches": formatted.join("\n"),
            "total_matches": match_indices.len(),
        });
    }

    // Line range mode or full file mode
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start = start_line.map(|s| s.saturating_sub(1)).unwrap_or(0);
    let end = end_line.map(|e| e.min(total_lines)).unwrap_or(total_lines);

    if start >= total_lines {
        return json!({ "error": format!("start_line {} exceeds file length ({})", start, total_lines) });
    }

    let selected: Vec<&str> = lines.get(start..end).unwrap_or(&lines[start..]).to_vec();
    let truncated = end - start > MAX_LINES;

    let result_lines: Vec<&str> = if truncated {
        selected.iter().take(MAX_LINES).copied().collect()
    } else {
        selected
    };

    let numbered = add_line_numbers(&result_lines.join("\n"), start + 1);

    json!({
        "type": "file",
        "path": path,
        "content": numbered,
        "start_line": start + 1,
        "end_line": (start + result_lines.len()).min(total_lines),
        "total_lines": total_lines,
        "truncated": truncated,
        "max_lines": MAX_LINES,
    })
}

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
                return not_found_error(original);
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
            return not_found_error(original);
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

        let mut failures: Vec<String> = Vec::new();

        for item in &arr {
            let path = match item["path"].as_str() {
                Some(p) => p.to_string(),
                None => {
                    results.push(json!({ "error": "missing path" }));
                    failures.push("<unknown>".to_string());
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

            let failed = write_result.get("error").is_some();
            results.push(write_result);

            if failed {
                failures.push(path.clone());
                continue; // don't add failed paths to autocheck
            }

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

        json!({ "results": results, "failed_paths": failures, "autochecks": autochecks })
    }

    async fn bash(&self, command: String, timeout_ms: Option<u64>) -> Value {
        run_bash(&command, timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)).await
    }

    /// Read, search, explore, or summarize files and directories.
    /// Designed to provide maximum context without exceeding LLM token limits.
    /// Always returns content prefixed with line numbers to assist future Write operations.
    ///
    /// Modes:
    ///   - directory path → returns a tree view of the directory (configurable depth)
    ///   - all modifiers omitted → read entire file (auto-truncates and warns if > max_lines)
    ///   - start_line / end_line present → read a specific line range (for pagination)
    ///   - search_regex present → grep mode: returns matched lines with `context_lines` around them
    ///   - outline_only = true → returns file skeleton (imports, class/function signatures only) via AST
    ///   - extract_symbol present → extracts the full body of a specific class or function via AST
    ///
    /// path: absolute path to the file or directory
    /// start_line: optional starting line number (1-indexed)
    /// end_line: optional ending line number
    /// search_regex: string or regex to search for in the file
    /// context_lines: number of lines to show before and after a regex match (default: 2)
    /// outline_only: boolean, if true, parses code and returns only structural signatures
    /// extract_symbol: exact name of a function, class, or method to extract
    /// max_depth: maximum depth for directory tree view (default: 3, max: 5)
    async fn read(
        &self,
        path: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
        search_regex: Option<String>,
        context_lines: Option<usize>,
        outline_only: Option<bool>,
        extract_symbol: Option<String>,
        max_depth: Option<usize>,
    ) -> Value {
        do_read(&path, start_line, end_line, search_regex, context_lines, outline_only, extract_symbol, max_depth).await
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
