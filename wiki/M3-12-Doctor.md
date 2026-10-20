# M3-12 — `mcpgw doctor`

**Story:** X · **Estimate:** 2 days · **Depends on:** M2-02
**Done when:** doctor finds and explains each of ten seeded misconfigurations.
**Status:** ✅ Done

## Using it

```
$ mcpgw doctor
  ok   config: ~/.config/mcpgw/config.toml (4 servers, 1 profiles)
  ok   vault: ~/.local/share/mcpgw/vault.json opens (key in the OS keychain), 6 secrets
 FAIL  server gh: uses the secret "gh_token", which isn't in the vault, so it can't start
        fix: `mcpgw secret set gh_token`
 info  server fs: @modelcontextprotocol/server-filesystem runs through npx on each start (network, slower)
        fix: `mcpgw runtimes install fs`
 warn  daemon: …/daemon.json describes a daemon that isn't running: process 4242 is gone
        fix: `mcpgw up` replaces it (or delete …/daemon.json)
1 problem(s), 1 warning(s), 2 ok
```

The command exits non-zero when anything fails. `mcpgw doctor --json` prints
the checks as `{id, subject, level, message, fix}` for tools and tests.
Doctor loads the config itself, so a file that doesn't parse is reported
rather than stopping the command.

## Checks

| Id | Level | Finds |
|----|-------|-------|
| `config_invalid` | fail | the config file doesn't load (with its line and column) |
| `vault_unreadable` | fail | the vault doesn't open with the current key source (wrong `MCPGW_VAULT_PASSPHRASE`, locked keychain) |
| `secret_missing` | fail | a server binds or references a secret that isn't stored (OAuth ones point to `mcpgw auth connect`) |
| `command_missing` | fail | a stdio server's command isn't a file or on `PATH` (a cached npx package needs `node`) |
| `package_not_cached` | info | an npx/uvx server that still resolves its package at each start (M3-08) |
| `container_runtime_missing` / `container_runtime_down` | fail | container servers without Docker/Podman, or with the daemon not answering |
| `port_in_use` | fail | `gateway.listen` is taken by another program (when the daemon isn't running) |
| `stale_daemon_file` | warn | `daemon.json` for a process that's gone or not answering |
| `server_quarantined` / `server_failing` | warn | from a running daemon: quarantined (drift, M3-10 scan) or failing servers, with the next command to run |
| `token_permissions` | warn | the gateway token file readable by others (Unix) |
| `oauth_login_needed` / `oauth_expired` | fail | a login whose refresh token was refused, or that expired with no refresh token |
| `profile_unknown_server` | warn | a profile naming a server that doesn't exist |
| `client_entry_broken` | fail | a Claude Code, Claude Desktop, Cursor or VS Code config whose mcpgw entry runs a program that doesn't exist |
| `client_other_config` | warn | that entry pointing at a different mcpgw config |
| `client_not_using_mcpgw` | info | a client with its own servers and no mcpgw entry (fix: `mcpgw client import`) |
| `client_config_unreadable` | warn | a client config that isn't valid JSON |
| `otlp_unreachable` | warn | nothing answering at `gateway.otlp_endpoint` (M3-11) |

## Tests

`crates/mcpgw/tests/doctor.rs` gives each run its own `HOME`, so real
client configs are never read.

- **Done-when:** ten seeded misconfigurations, one per scenario, are each
  found at the right level, with an explanation and a fix:
  1. a config that doesn't parse (`listen = 7777`: "expected socket
     address" at 2:10);
  2. a server command that isn't installed;
  3. a missing secret;
  4. the listen port held by another socket;
  5. a stale daemon file (process gone);
  6. a container server with no Docker or Podman on `PATH`;
  7. an expired OAuth login with no refresh token;
  8. a profile naming a server that doesn't exist;
  9. a Cursor config whose mcpgw entry runs `/no/such/dir/mcpgw`;
  10. the wrong vault passphrase.
- A healthy setup (a stored secret, a working server) passes with "0
  problem(s)". The help test now covers `doctor`, `scan`, `stats` and
  `runtimes install`.
