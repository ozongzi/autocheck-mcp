use agentix::{LlmEvent, McpServer, Message, Provider, Request, tool};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use autocheck_mcp::languages::{CheckResult, Language, detect_language, get_support};
use autocheck_mcp::utils::{
    BashOutput, DEFAULT_TIMEOUT_MS, OUTPUT_LIMIT, find_root, run_bash, run_bash_streaming,
};

// ── file context snippet ──────────────────────────────────────────────────────

fn not_found_error(content: String) -> Value {
    let total = content.len();
    let mut cut = OUTPUT_LIMIT.min(total);
    while !content.is_char_boundary(cut) {
        cut -= 1;
    }
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
    entries.sort_by_key(|a| a.file_name());

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
        result.push_str(&format!(
            "{}└── ... ({} more items)\n",
            prefix,
            total - MAX_DIR_ITEMS
        ));
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
                let kind = parts
                    .get(3)
                    .and_then(|s| s.strip_prefix("kind:"))
                    .unwrap_or("?")
                    .to_string();
                let line_num = parts
                    .last()
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

    let symbol_lines = lines
        .get(target_line - 1..end_line.saturating_sub(1))?
        .join("\n");

    Some(symbol_lines)
}

fn add_line_numbers(content: &str, start_line: usize) -> String {
    content
        .lines()
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

async fn auto_check(path: &str) -> Option<CheckResult> {
    let p = Path::new(path);
    let lang = detect_language(p)?;
    let support = get_support(lang);
    let root = find_root(p, support.root_markers())?;
    Some(support.run_check(&root, Some(p)).await)
}

// ── tools implementation ──────────────────────────────────────────────────────

async fn do_write(
    path: &str,
    new_string: String,
    old_string: Option<String>,
    count: Option<usize>,
    append: Option<bool>,
    shebang: Option<String>,
) -> Value {
    let new_string = if let Some(ref shebang) = shebang {
        let line = if shebang.starts_with("#!") {
            shebang.clone()
        } else {
            format!("#!{shebang}")
        };
        format!("{line}\n{new_string}")
    } else {
        new_string
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
            let updated = original.replacen(anchor.as_str(), &format!("{anchor}{new_string}"), 1);
            if let Err(e) = std::fs::write(path, &updated) {
                return json!({ "error": format!("write failed: {e}") });
            }
            let run = if shebang.is_some() {
                Some(run_executable(path).await)
            } else {
                None
            };
            let mut r = json!({ "inserted_after": path, "bytes": new_string.len() });
            if let Some(run) = run {
                r["run"] = run;
            }
            return r;
        }
        use std::fs::OpenOptions;
        match OpenOptions::new().create(true).append(true).open(path) {
            Err(e) => return json!({ "error": format!("open failed: {e}") }),
            Ok(mut f) => {
                if let Err(e) = f.write_all(new_string.as_bytes()) {
                    return json!({ "error": format!("write failed: {e}") });
                }
            }
        }
        let run = if shebang.is_some() {
            Some(run_executable(path).await)
        } else {
            None
        };
        let mut r = json!({ "appended": path, "bytes": new_string.len() });
        if let Some(run) = run {
            r["run"] = run;
        }
        return r;
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
            original.replace(old.as_str(), &new_string)
        } else {
            let mut s = original.clone();
            for _ in 0..expected {
                s = s.replacen(old.as_str(), &new_string, 1);
            }
            s
        };
        if let Err(e) = std::fs::write(path, &updated) {
            return json!({ "error": format!("write failed: {e}") });
        }
        let run = if shebang.is_some() {
            Some(run_executable(path).await)
        } else {
            None
        };
        let mut r = json!({ "replaced": path, "occurrences": found });
        if let Some(run) = run {
            r["run"] = run;
        }
        return r;
    }

    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return json!({ "error": format!("mkdir failed: {e}") });
    }
    if let Err(e) = std::fs::write(path, &new_string) {
        return json!({ "error": format!("write failed: {e}") });
    }
    let run = if shebang.is_some() {
        Some(run_executable(path).await)
    } else {
        None
    };
    let mut r = json!({ "written": path, "bytes": new_string.len() });
    if let Some(run) = run {
        r["run"] = run;
    }
    r
}

struct Tools;

#[tool]
impl agentix::Tool for Tools {
    /// Read multiple files or directories in one call to reduce round-trips.
    /// Each item has the same fields as the `read` tool:
    ///   path (required), start_line, end_line, search_regex, context_lines, outline_only, extract_symbol, max_depth
    async fn multiread(&self, reads: Vec<Value>) -> Value {
        let mut results = Vec::new();
        for item in &reads {
            let path = match item["path"].as_str() {
                Some(p) => p.to_string(),
                None => {
                    results.push(json!({ "error": "missing path" }));
                    continue;
                }
            };
            let result = do_read(
                &path,
                item["start_line"].as_u64().map(|n| n as usize),
                item["end_line"].as_u64().map(|n| n as usize),
                item["search_regex"].as_str().map(str::to_string),
                item["context_lines"].as_u64().map(|n| n as usize),
                item["outline_only"].as_bool(),
                item["extract_symbol"].as_str().map(str::to_string),
                item["max_depth"].as_u64().map(|n| n as usize),
            )
            .await;
            results.push(result);
        }

        json!({ "results": results })
    }

    /// Write multiple files in one call, then run checks (autocheck) for all affected projects once at the end.
    /// Each item has the same fields as the `write` tool:
    ///   path (required), new_string (required), old_string, count, append, shebang
    async fn multiwrite(&self, writes: Vec<Value>) -> Value {
        let mut results = Vec::new();
        let mut affected: HashSet<(PathBuf, Language)> = HashSet::new();

        let mut failures: Vec<String> = Vec::new();

        for item in &writes {
            let path = match item["path"].as_str() {
                Some(p) => p.to_string(),
                None => {
                    results.push(json!({ "error": "missing path" }));
                    failures.push("<unknown>".to_string());
                    continue;
                }
            };
            let new_string = item["new_string"].as_str().unwrap_or("").to_string();
            let old_string = item["old_string"].as_str().map(str::to_string);
            let count = item["count"].as_u64().map(|n| n as usize);
            let append = item["append"].as_bool();
            let shebang = item["shebang"].as_str().map(str::to_string);

            let write_result =
                do_write(&path, new_string, old_string, count, append, shebang).await;

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

    #[streaming]
    fn bash(&self, command: String, timeout_ms: Option<u64>) {
        async_stream::stream! {
            use agentix::ToolOutput;
            use futures::StreamExt;
            let mut stream = run_bash_streaming(command, timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
            while let Some(item) = stream.next().await {
                match item {
                    BashOutput::Line(line) => yield ToolOutput::Progress(line),
                    BashOutput::Done(result) => yield ToolOutput::Result(result),
                }
            }
        }
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
        do_read(
            &path,
            start_line,
            end_line,
            search_regex,
            context_lines,
            outline_only,
            extract_symbol,
            max_depth,
        )
        .await
    }

    /// Write to a file (overwrite / replace / append), then run language-specific check (autocheck).
    /// Can also used for normal text manipulation tasks, not necessarily code files.
    ///
    /// Modes:
    ///   - old_string omitted, append omitted → overwrite entire file with new_string (or create)
    ///   - old_string present, append omitted → replace occurrences of old_string with new_string
    ///   - append = true, old_string omitted  → append new_string to end of file
    ///   - append = true, old_string present  → insert new_string immediately after old_string
    ///
    /// path: absolute path to the file
    /// new_string: content to write, replacement text, or text to insert
    /// old_string: exact text to find and replace (or anchor for insert-after)
    /// count: expected number of replacements when using old_string in replace mode (default 1, 0 = replace all)
    /// append: if true, append to end of file or insert after old_string
    /// shebang: if provided, prepend this shebang line and execute the file after writing
    async fn write(
        &self,
        path: String,
        new_string: String,
        old_string: Option<String>,
        count: Option<usize>,
        append: Option<bool>,
        shebang: Option<String>,
    ) -> Value {
        let mut result = do_write(&path, new_string, old_string, count, append, shebang).await;
        if let Some(ac) = auto_check(&path).await {
            result["autocheck"] = ac.to_json();
        }
        result
    }

    /// Compare two files and return their differences.
    /// path1: path to the first file
    /// path2: path to the second file
    async fn diff(&self, path1: String, path2: String) -> Value {
        let text1 = std::fs::read_to_string(&path1).unwrap_or_default();
        let text2 = std::fs::read_to_string(&path2).unwrap_or_default();
        use similar::TextDiff;
        let diff = TextDiff::from_lines(&text1, &text2);
        let result = diff
            .unified_diff()
            .header(&format!("a/{path1}"), &format!("b/{path2}"))
            .to_string();
        if result.is_empty() {
            json!({ "diff": "(no changes)" })
        } else if result.len() > OUTPUT_LIMIT {
            let mut cut = OUTPUT_LIMIT;
            while !result.is_char_boundary(cut) {
                cut -= 1;
            }
            let truncated = format!("{}... (truncated)", &result[..cut]);
            json!({ "diff": truncated })
        } else {
            json!({ "diff": result })
        }
    }
}

// ── master tools (DeepSeek sub-agent) ────────────────────────────────────────

const MASTER_READ_SYSTEM: &str = "\
You are a precision file-intelligence sub-agent. Your sole purpose is to answer \
the user's read goal with the highest possible accuracy.\n\
\n\
Mandatory rules — follow every one:\n\
1. NEVER guess or invent file contents. Use the `read` tool before answering.\n\
2. Prefer targeted reads: `search_regex`, `extract_symbol`, or line ranges over \
   full-file reads when the goal is narrow.\n\
3. Start with a directory read to orient yourself if the target path is unclear.\n\
4. When the goal mentions a symbol or function, use `extract_symbol`.\n\
5. Return ONLY verified content copied verbatim from the files — include file \
   paths and line numbers so every claim is traceable.\n\
6. If the content cannot be found after thorough searching, say so explicitly. \
   Never hallucinate.\n\
\n\
RESPONSE FORMAT — this is an absolute constraint:\n\
- Your entire response must be exactly one JSON object, nothing else.\n\
- No markdown fences, no prose before or after, no explanation outside the object.\n\
- Schema: {\"success\": <boolean>, \"description\": \"<string>\"}\n\
- success=true  → description contains the exact content / findings.\n\
- success=false → description explains why the content could not be found.\n\
- Any text outside the JSON object will cause a hard parse failure.";

const MASTER_WRITE_SYSTEM: &str = "\
You are a precision file-editing sub-agent. Your sole purpose is to apply the \
user's write goal as a minimal, correct change.\n\
\n\
Mandatory rules — follow every one:\n\
1. Read the target file FIRST with the `read` tool to understand its exact \
   current content and structure before generating any edit.\n\
2. Use `write` in replace mode (`old_string` → `new_string`) for surgical edits. \
   The `old_string` must be copied verbatim from the file — never paraphrase.\n\
3. Make the smallest change that fully satisfies the goal. Do not refactor \
   surrounding code unless the goal explicitly requires it.\n\
4. After writing, re-read the modified region to verify the result looks correct.\n\
5. If the goal is ambiguous, resolve it by reading first, then apply the most \
   conservative interpretation.\n\
\n\
RESPONSE FORMAT — this is an absolute constraint:\n\
- Your entire response must be exactly one JSON object, nothing else.\n\
- No markdown fences, no prose before or after, no explanation outside the object.\n\
- Schema: {\"success\": <boolean>, \"description\": \"<string>\"}\n\
- success=true  → description summarises what was changed.\n\
- success=false → description explains why the edit could not be applied.\n\
- Any text outside the JSON object will cause a hard parse failure.";

struct MasterTools {
    api_key: String,
}

#[tool]
impl agentix::Tool for MasterTools {
    /// Semantic read: describe in plain language what you need to find or understand.
    /// A DeepSeek sub-agent will use the `read` tool to locate and return the exact
    /// content — it never guesses. Streams the agent's reasoning and tool calls live.
    ///
    /// goal: natural-language description of what to read, find, or summarize
    #[streaming]
    fn master_read(&self, goal: String) {
        let api_key = self.api_key.clone();
        async_stream::stream! {
            use agentix::tool_trait::ToolOutput;
            use futures::StreamExt;

            let file_tools = Tools;
            let raw_defs = file_tools.raw_tools();
            let http = reqwest::Client::new();

            let mut req = Request::new(Provider::DeepSeek, api_key)
                .system_prompt(MASTER_READ_SYSTEM)
                .user(format!("Read goal: {goal}"))
                .tools(raw_defs)
                .json();

            loop {
                let mut event_stream = match req.stream(&http).await {
                    Ok(s) => s,
                    Err(e) => {
                        yield ToolOutput::Result(json!({ "error": format!("DeepSeek error: {e}") }));
                        return;
                    }
                };

                let mut tool_calls: Vec<agentix::request::ToolCall> = Vec::new();
                let mut content = String::new();

                while let Some(event) = event_stream.next().await {
                    match event {
                        LlmEvent::Token(t) => {
                            yield ToolOutput::Progress(t.clone());
                            content.push_str(&t);
                        }
                        LlmEvent::ToolCall(tc) => {
                            tool_calls.push(tc);
                        }
                        LlmEvent::Error(e) => {
                            yield ToolOutput::Result(json!({ "error": e }));
                            return;
                        }
                        _ => {}
                    }
                }

                if tool_calls.is_empty() {
                    // Try to parse the whole content, then fall back to extracting
                    // the first {...} block in case the model leaked surrounding text.
                    let result: Value = serde_json::from_str(&content).ok()
                        .or_else(|| {
                            let start = content.find('{')?;
                            let end   = content.rfind('}')?;
                            serde_json::from_str(&content[start..=end]).ok()
                        })
                        .unwrap_or_else(|| json!({ "success": false, "description": content }));
                    yield ToolOutput::Result(result);
                    return;
                }

                req = req.message(Message::Assistant {
                    content: if content.is_empty() { None } else { Some(content) },
                    reasoning: None,
                    tool_calls: tool_calls.clone(),
                });

                for tc in &tool_calls {
                    yield ToolOutput::Progress(format!("\n[tool:{}] {}\n", tc.name, tc.arguments));
                    let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(Value::Null);
                    let mut ts = file_tools.call(&tc.name, args).await;
                    let mut result = Value::Null;
                    while let Some(out) = ts.next().await {
                        match out {
                            ToolOutput::Progress(p) => yield ToolOutput::Progress(p),
                            ToolOutput::Result(r) => result = r,
                        }
                    }
                    req = req.message(Message::ToolResult {
                        call_id: tc.id.clone(),
                        content: result.to_string(),
                    });
                }
            }
        }
    }

    /// Semantic write: describe in plain language what change you want to make.
    /// A DeepSeek sub-agent reads the target file first, then applies the minimal
    /// correct edit using the `write` tool. Streams live progress and the autocheck result.
    ///
    /// Run a shell command and stream its output line by line.
    /// command: the shell command to execute
    /// timeout_ms: optional timeout in milliseconds
    #[streaming]
    fn bash(&self, command: String, timeout_ms: Option<u64>) {
        async_stream::stream! {
            use agentix::ToolOutput;
            use futures::StreamExt;
            let mut stream = run_bash_streaming(command, timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
            while let Some(item) = stream.next().await {
                match item {
                    BashOutput::Line(line) => yield ToolOutput::Progress(line),
                    BashOutput::Done(result) => yield ToolOutput::Result(result),
                }
            }
        }
    }

    /// goal: natural-language description of the edit — what to change and where
    #[streaming]
    fn master_write(&self, goal: String) {
        let api_key = self.api_key.clone();
        async_stream::stream! {
            use agentix::tool_trait::ToolOutput;
            use futures::StreamExt;

            let file_tools = Tools;
            let raw_defs = file_tools.raw_tools();
            let http = reqwest::Client::new();

            let mut req = Request::new(Provider::DeepSeek, api_key)
                .system_prompt(MASTER_WRITE_SYSTEM)
                .user(format!("Write goal: {goal}"))
                .tools(raw_defs)
                .json();

            loop {
                let mut event_stream = match req.stream(&http).await {
                    Ok(s) => s,
                    Err(e) => {
                        yield ToolOutput::Result(json!({ "error": format!("DeepSeek error: {e}") }));
                        return;
                    }
                };

                let mut tool_calls: Vec<agentix::request::ToolCall> = Vec::new();
                let mut content = String::new();

                while let Some(event) = event_stream.next().await {
                    match event {
                        LlmEvent::Token(t) => {
                            yield ToolOutput::Progress(t.clone());
                            content.push_str(&t);
                        }
                        LlmEvent::ToolCall(tc) => {
                            tool_calls.push(tc);
                        }
                        LlmEvent::Error(e) => {
                            yield ToolOutput::Result(json!({ "error": e }));
                            return;
                        }
                        _ => {}
                    }
                }

                if tool_calls.is_empty() {
                    // Try to parse the whole content, then fall back to extracting
                    // the first {...} block in case the model leaked surrounding text.
                    let mut result: Value = serde_json::from_str(&content).ok()
                        .or_else(|| {
                            let start = content.find('{')?;
                            let end   = content.rfind('}')?;
                            serde_json::from_str(&content[start..=end]).ok()
                        })
                        .unwrap_or_else(|| json!({ "success": false, "description": content }));
                    if result.get("success").and_then(Value::as_bool) == Some(true) {
                        result.as_object_mut().map(|o| o.remove("description"));
                    }
                    yield ToolOutput::Result(result);
                    return;
                }

                req = req.message(Message::Assistant {
                    content: if content.is_empty() { None } else { Some(content) },
                    reasoning: None,
                    tool_calls: tool_calls.clone(),
                });

                for tc in &tool_calls {
                    yield ToolOutput::Progress(format!("\n[tool:{}] {}\n", tc.name, tc.arguments));
                    let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(Value::Null);
                    let mut ts = file_tools.call(&tc.name, args).await;
                    let mut result = Value::Null;
                    while let Some(out) = ts.next().await {
                        match out {
                            ToolOutput::Progress(p) => yield ToolOutput::Progress(p),
                            ToolOutput::Result(r) => result = r,
                        }
                    }
                    req = req.message(Message::ToolResult {
                        call_id: tc.id.clone(),
                        content: result.to_string(),
                    });
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    McpServer::new(MasterTools {
        api_key: std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set"),
    })
    .with_name("autocheck-mcp")
    .serve_stdio()
    .await?;
    Ok(())
}
