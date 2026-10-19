# M1-01 — Config file loading, defaults, validation

**Crate:** `mcpgw-core` (`src/config.rs`) · **Status:** ✅ Done

## What it adds
- `Config::default_path()` — `$MCPGW_CONFIG`, else `$XDG_CONFIG_HOME/mcpgw/config.toml`, else `~/.config/mcpgw/config.toml`.
- `Config::load_or_default(path)` — a missing file yields defaults; a malformed file is an error.
- `Config::to_toml_string()` — round-trips (used by `add-server`).
- `[gateway] store_path` / `token_path` with defaults under `data_dir()` (`$XDG_DATA_HOME/mcpgw` or `~/.local/share/mcpgw`).
- Per-server `enabled` (default `true`) and stdio `env` table; `Config::enabled_servers()`.

## Validation rules
- Server names: 1–64 chars of `[A-Za-z0-9_-]`, must not contain `__` (the tool-prefix separator, `TOOL_SEPARATOR`) or start/end with `_`. This guarantees `server__tool` names split unambiguously (M1-04).
- Names unique; stdio `command` / http `url` non-empty; URLs must be `http://` or `https://`.
- Unknown top-level/gateway keys are rejected.

## Example
```toml
[gateway]
allow_agent_add_servers = false   # locked default
# store_path = "/path/mcpgw.db"
# token_path = "/path/token"

[search]
embeddings = true                 # locked default

[[server]]
name = "fs"
transport = "stdio"
command = "mcp-fs"
args = ["--ro"]
env = { LOG = "warn" }

[[server]]
name = "web"
transport = "http"
url = "https://example.com/mcp"
enabled = false
```

## Tests
9 unit tests: locked defaults, parsing, duplicates/unknown fields, empty command, name rules, `enabled`/`env`/paths, bad URL scheme, load-or-default + round-trip.
