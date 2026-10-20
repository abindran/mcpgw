# M1-13 — CLI

**Story:** US3 · **Estimate:** 3 days · **Depends on:** M1-06, M1-11
**Done when:** each command has help text and an integration test.
**Status:** ✅ Done

## Commands

| Command | What it does |
|---------|--------------|
| `serve [--stdio]` | run the daemon in the foreground (or a single stdio client) |
| `up` / `down` | start the daemon detached / stop it and wait for it to exit |
| `status` | daemon pid, URL and version; per server: state, tool count, pid, last error |
| `connect` | stdio shim (M1-02) |
| `add <name> --command …` (`add-server`) | store, introspect and pin (resolving `--secret VAR=name`), and hot-add to a running daemon. Also `--env`, `--alias`, `--stateful`, `--disabled`, `--no-introspect` |
| `remove <name>` (`remove-server`, `rm`) | delete, also from a running daemon |
| `enable` / `disable <name>` | persist the flag; a running daemon applies it at once |
| `list [--check]` (`list-servers`, `ls`) | configured servers; `--check` starts each one |
| `logs [name] [-n N]` | a server's recent stderr (redacted), or the daemon log |
| `pin <name>` | accept a quarantined server's new tools (M1-08); without a daemon, re-captures and stores the pin |
| `restart <name>` | clear a `Failed` server; it starts fresh on its next call |
| `secret set <name> [--description]` | value read hidden from the terminal, or from stdin (never a flag, so it stays out of shell history) |
| `secret ls` / `secret rm <name>` | names and update times only / delete |
| `token [--rotate]` | the loopback bearer token |

Config-file servers can't be changed from the CLI; the error names the file.

## Daemon control API (`mcpgw-api::admin`)

The CLI talks to a running daemon over token-protected `/admin` routes on
the same edge as `/mcp` (Host and Origin checks plus bearer token). These
grow into the versioned REST API in M2-11.

| Route | Effect |
|-------|--------|
| `GET /admin/status` | daemon and server status |
| `POST /admin/servers` | add a server (with its pinned manifest) |
| `DELETE /admin/servers/{name}` | remove |
| `POST /admin/servers/{name}/{enable,disable,restart,pin}` | lifecycle |
| `GET /admin/servers/{name}/logs?lines=N` | stderr tail |
| `POST /admin/shutdown` | graceful stop (`mcpgw down`) |

`mcpgw secret` keeps `SecretMeta` (description, times) in the store next to
the vault. The binary is now split into `serve`, `control`, `servers`,
`secrets` and `daemon` modules.

## Tests (`crates/mcpgw/tests/commands.rs`)

- `every_command_has_help`: each of the 17 commands' `--help` describes it.
- `daemon_server_and_secret_commands`, one session through the real binary
  and daemon: `status` (not running) → `secret set` from stdin → `secret ls`
  (name and description, no value) → `add` with `--secret` (introspected with
  the secret) → `up` (twice) → `status` → an MCP client calls the server and
  gets the redacted secret → `status` shows it idle with a pid → `logs fx`
  and `logs` → `add` while running (the client sees the new tools) →
  `disable` / `enable` live → `list` → `pin` (refused: not quarantined) →
  `restart` → `remove` live → `secret rm` → `down` → `status`.
- `errors_name_the_problem`: unknown server, no daemon, empty or invalid
  secret.
