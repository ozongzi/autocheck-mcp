# autocheck-mcp

An MCP server that gives AI coding agents a **read / write / bash** toolkit with automatic language checks after every file edit.

After writing a `.rs`, `.go`, `.py`, `.js`, `.ts`, `.jsx`, or `.tsx` file the server immediately runs the project's linter/type-checker and returns structured diagnostics — errors, warnings, and source context — so the agent can fix mistakes in the same turn without an extra round-trip.

---

## Tools

### `read`

Read a file, directory tree, or search within a file.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | — | Absolute path to a file or directory |
| `start_line` | `int?` | — | First line to read (1-indexed) |
| `end_line` | `int?` | — | Last line to read (inclusive) |
| `search_regex` | `string?` | — | Regex to search; returns matching lines with context |
| `context_lines` | `int?` | `2` | Lines of context around each regex match |
| `outline_only` | `bool?` | `false` | Return symbol outline via ctags (functions, classes, …) |
| `extract_symbol` | `string?` | — | Extract the full body of a named symbol via ctags |
| `max_depth` | `int?` | `3` | Max depth for directory tree view (max 5) |

**Modes** (selected by which parameters are set):

- **Directory** — `path` points to a directory → returns a tree view
- **Full file** — only `path` → returns entire file with line numbers (auto-truncates at 1000 lines)
- **Line range** — `start_line` / `end_line` → paginate large files
- **Regex search** — `search_regex` → grep with surrounding context
- **Outline** — `outline_only = true` → symbols and their line numbers (requires `ctags`)
- **Symbol extract** — `extract_symbol = "MyFunction"` → full body of that symbol (requires `ctags`)

---

### `write`

Write, replace, or append to a file. Runs autocheck after every edit.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | — | Absolute path (parent dirs created automatically) |
| `new_string` | `string` | — | Content to write / replacement text / text to insert |
| `old_string` | `string?` | — | Exact text to find and replace (or anchor for insert-after) |
| `count` | `int?` | `1` | Expected replacement count (`0` = replace all occurrences) |
| `append` | `bool?` | `false` | Append to end of file, or insert after `old_string` |
| `shebang` | `string?` | — | Prepend shebang line and execute the file after writing |

**Modes:**

- `old_string` omitted, `append` omitted → **overwrite** entire file (or create)
- `old_string` present, `append` omitted → **replace** occurrences of `old_string`
- `append = true`, `old_string` omitted → **append** to end of file
- `append = true`, `old_string` present → **insert** `new_string` immediately after `old_string`

**Autocheck** fires for: `.rs`, `Cargo.toml`, `.go`, `go.mod`, `.py`, `.js`, `.ts`, `.jsx`, `.tsx`, `package.json`

---

### `bash`

Run a shell command and stream its output line by line.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `command` | `string` | — | Shell command to execute |
| `timeout_ms` | `int?` | `10000` | Timeout in milliseconds |

stdout and stderr are combined. Output is truncated to 8000 characters.

---

### `diff`

Compare two files and return their unified diff.

| Parameter | Type | Description |
|---|---|---|
| `path1` | `string` | Absolute path to first file |
| `path2` | `string` | Absolute path to second file |

---

## Autocheck — language support

| Language | Triggered by | Tool used |
|---|---|---|
| **Rust** | `.rs`, `Cargo.toml` | `cargo clippy --fix` → `cargo clippy` |
| **Go** | `.go`, `go.mod` | `go vet` |
| **Python** | `.py` | `ruff check` (falls back to `pyflakes`) |
| **JavaScript / TypeScript** | `.js`, `.ts`, `.jsx`, `.tsx`, `package.json` | `tsc --noEmit` (if `tsconfig.json` present), then `eslint` or `biome` |

The check result is a structured JSON object:

```json
{
  "success": true,
  "fix_ok": false,
  "summary": "✓ JS/TS check passed: 0 error(s), 0 warning(s)",
  "errors": [],
  "warnings": [
    {
      "file": "/workspace/src/app.ts",
      "line": 12,
      "col": 5,
      "level": "warning",
      "message": "...",
      "source_context": { ... }
    }
  ]
}
```

---

## Installation

### Docker (recommended)

The image ships with **Rust + Clippy**, **Go**, **Bun**, **uv (Python)**, and **ctags** pre-installed.

**Build:**

```bash
docker build -t autocheck-mcp .
```

**Configure your MCP client** (e.g. Claude Desktop — `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):

```json
{
  "mcpServers": {
    "autocheck-mcp": {
      "command": "docker",
      "args": [
        "run", "--rm", "-i",
        "-v", "/your/project:/workspace",
        "autocheck-mcp"
      ]
    }
  }
}
```

Replace `/your/project` with the absolute path to your project. It will appear as `/workspace` inside the container.

**Windsurf / Cursor / other editors** — use the same JSON structure in the editor's MCP settings.

---

### Local (no Docker)

Requirements: Rust toolchain, plus any language tools you want checks for (`go`, `ruff`/`pyflakes`, `tsc`, `ctags`).

```bash
cargo install --path .
```

Then in your MCP config:

```json
{
  "mcpServers": {
    "autocheck-mcp": {
      "command": "autocheck-mcp"
    }
  }
}
```

---

### Per-user containers (server deployments)

Start a long-lived container per user:

```bash
docker run -d --name user-abc \
  -v /data/users/abc:/workspace \
  --entrypoint sleep autocheck-mcp infinity
```

Point the MCP client at it:

```json
{
  "mcpServers": {
    "autocheck-mcp": {
      "command": "docker",
      "args": ["exec", "-i", "user-abc", "autocheck-mcp"]
    }
  }
}
```

---

## `--master` mode (experimental)

Pass a DeepSeek API key to unlock a `master_write` tool — describe what you want changed in plain language and a sub-agent reads the file and applies the minimal correct edit automatically:

```json
{
  "mcpServers": {
    "autocheck-mcp": {
      "command": "autocheck-mcp",
      "args": ["--master", "sk-..."]
    }
  }
}
```

---

## License

MIT OR Apache-2.0
