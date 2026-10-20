# M1-14 — `client install` and `client import`

**Story:** US3 · **Estimate:** 2 days · **Depends on:** M1-13
**Done when:** import moves existing `mcpServers` entries into mcpgw and writes one gateway entry.
**Status:** ✅ Done

## Commands

```sh
mcpgw client install <client> [--path FILE] [--dry-run]
mcpgw client import  <client> [--path FILE] [--dry-run] [--keep-env]
```

`<client>`: `claude-code`, `claude-desktop`, `cursor`, `vscode`.

| Client | Default file | Server map |
|--------|--------------|------------|
| Claude Code | `~/.claude.json` | `mcpServers` |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS), `%APPDATA%\Claude\…` (Windows), `~/.config/Claude/…` (Linux) | `mcpServers` |
| Cursor | `~/.cursor/mcp.json` | `mcpServers` |
| VS Code | `…/Code/User/mcp.json` | `servers` |

Use `--path` for project files (`.mcp.json`, `.cursor/mcp.json`,
`.vscode/mcp.json`).

The gateway entry runs the stdio shim with the absolute path of the
binary, plus `--config <file>` when a non-default config was given:
`{"command": "/…/mcpgw", "args": ["connect"]}`. VS Code entries also carry
`"type": "stdio"`. No token is written into client files.

## Import

1. Reads the file. JSONC is accepted (VS Code allows comments and trailing
   commas); a missing file counts as empty.
2. Converts every entry except an existing `mcpgw` one: stdio (`command`,
   `args`, `env`, `cwd`) or http (`url`, `headers`), and `disabled`. Names are
   made valid mcpgw names (`github tools` → `github-tools`) and kept unique
   (`fs`, `fs-2`, …).
3. **Moves credentials to the vault.** Env vars and headers whose names look
   like credentials (`*TOKEN*`, `*SECRET*`, `*PASSWORD*`, `*API_KEY*`,
   `Authorization`, …) become secrets named `<server>_<var>` and are bound
   back: env through `secrets`, headers through `${secret:…}`. Nothing is
   left in plaintext in mcpgw's store. `--keep-env` skips this.
4. Stores the servers and hot-adds them to a running daemon.
5. Backs up the original file (`<file>.mcpgw-backup-<unix time>`), then
   rewrites it with **only** the `mcpgw` entry in the server map. Every
   other key is kept (`numStartups`, `projects`, `inputs`, …); comments are
   not.

`--dry-run` prints the mapping (`github tools -> github-tools [stdio] (secrets
to vault: github_tools_github_token)`) and changes nothing. A second import
finds nothing left to move.

`install` adds or replaces only the `mcpgw` entry and keeps the client's
other servers.

## Tests (`crates/mcpgw/tests/clients.rs`)

- **Claude Code import**: four entries (stdio with a token, plain stdio, http
  with `Authorization`, an old `mcpgw` entry). The dry run changes nothing.
  After the import the file keeps its other keys, has exactly one server
  (`mcpgw … --config <abs> connect`), contains no credential, and a backup
  holds the original. `list` shows the three servers; `secret ls` shows both
  moved credentials; the SQLite file doesn't contain the token. Through
  `serve --stdio`, the imported server receives its token and returns
  `[REDACTED:github_tools_github_token]`. A re-import is a no-op.
- **VS Code JSONC** with comments and trailing commas imports, keeping
  `inputs`.
- **install** keeps other servers, `--dry-run` writes nothing, and a missing
  file is created.
- `mcpgw-config::clients` units: JSONC stripping (including in-string
  `//`, `/*` and non-ASCII), name sanitising, entry conversion, credential
  extraction, entry install and replace.
