use regex::Regex;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn parse_rust_diagnostics(stderr: &str, crate_root: &Path) -> Vec<Value> {
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

        let mut location: Option<(String, usize, usize)> = None;
        let mut j = i + 1;
        while j < lines.len() && j < i + 6 {
            let loc = lines[j].trim();
            if let Some(rest) = loc.strip_prefix("--> ") {
                let p: Vec<&str> = rest.splitn(3, ':').collect();
                if p.len() >= 2 {
                    if let Ok(row) = p[1].parse::<usize>() {
                        let col = p.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
                        location = Some((p[0].to_string(), row, col));
                    }
                }
                break;
            }
            if lines[j].starts_with("error") || lines[j].starts_with("warning") {
                break;
            }
            j += 1;
        }

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

pub fn parse_generic_diagnostics(output: &str, root: &Path) -> Vec<Value> {
    let mut diags = Vec::new();
    // Match common formats:
    // path/to/file:line:col: message
    // path/to/file:line: message
    let re = Regex::new(r"(?m)^(.+?):(\d+)(?::(\d+))?:?\s*(.*)$").unwrap();

    for cap in re.captures_iter(output) {
        let file = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let line = cap.get(2).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
        let col = cap.get(3).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
        let message = cap.get(4).map(|m| m.as_str().to_string()).unwrap_or_default();

        if file.is_empty() || message.is_empty() {
            continue;
        }

        // Check if file exists relative to root or absolute
        let abs_path = if Path::new(&file).is_absolute() {
            PathBuf::from(&file)
        } else {
            root.join(&file)
        };

        if !abs_path.exists() {
            continue;
        }

        let mut diag = json!({
            "file": file,
            "line": line,
            "col": col,
            "message": message,
            "level": if message.to_lowercase().contains("error") { "error" } else { "warning" }
        });

        // Try to get source context
        if let Ok(src) = std::fs::read_to_string(&abs_path) {
            let src_lines: Vec<&str> = src.lines().collect();
            let total = src_lines.len();
            let center = line.saturating_sub(1);
            if center < total {
                let start = center.saturating_sub(2);
                let end = (center + 3).min(total);
                let snippet: Vec<String> = src_lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(idx, l)| {
                        let lineno = start + idx + 1;
                        let marker = if lineno == line { ">>>" } else { "   " };
                        format!("{marker} {lineno:4} | {l}")
                    })
                    .collect();
                diag["source_context"] = json!({
                    "file": file,
                    "line": line,
                    "snippet": snippet.join("\n")
                });
            }
        }

        diags.push(diag);
    }

    diags
}
