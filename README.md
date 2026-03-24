# autocheck-mcp

MCP server with `bash` + `write` tools. After every `.rs` / `Cargo.toml` edit,
runs `cargo clippy --fix` + `cargo clippy (include check)` and returns structured diagnostics with
source context.

## Docker usage (recommended)

The image includes **Rust + Clippy**, **Bun**, and **uv** out of the box.

### 1. Build

```bash
docker build -t autocheck-mcp .
```

### 2. Configure Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`
(macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

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

Replace `/your/project` with the absolute path to your project directory.
Inside the container it appears as `/workspace`.

### Per-user containers (server deployments)

Start a long-lived container per user:

```bash
docker run -d --name user-abc \
  -v /data/users/abc:/workspace \
  --entrypoint sleep autocheck-mcp infinity
```

Then point Claude Desktop at:

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

## Local usage (no Docker)

```bash
cargo install --path .
```

Then in Claude Desktop:

```json
{
  "mcpServers": {
    "autocheck-mcp": {
      "command": "autocheck-mcp"
    }
  }
}
```

## Tools

| Tool | Description |
|---|---|
| `bash(command, timeout_ms?)` | Run a shell command. stdout+stderr combined, truncated to 8000 chars. Default timeout 10s. |
| `write(path, new_string, old_string?, count?, append?)` | Write/replace/append a file. Runs clippy+check for `.rs` and `Cargo.toml` files. |
