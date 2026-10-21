# The macOS app

`mcpgw.app` is a native SwiftUI front end for the gateway. It carries its
own `mcpgw` binary (`mcpgw.app/Contents/MacOS/mcpgw`), so there's nothing
else to install. It uses the same config, store, vault and daemon as the
CLI: anything done in one shows up in the other.

## Install

Download `mcpgw-<version>-macos-universal-app.zip` from the release, unzip
it and move `mcpgw.app` to `/Applications`. The app is signed ad hoc, not
notarized, so macOS blocks the first open. Either right-click it and
choose **Open**, or clear the quarantine flag:

```sh
xattr -dr com.apple.quarantine /Applications/mcpgw.app
```

To build it yourself you need Xcode 16 or later (Swift 6) and Rust:

```sh
macos/build.sh                 # this Mac's architecture, into target/macos/mcpgw.app
macos/build.sh --universal     # arm64 + x86_64 (both Rust targets installed)
macos/build.sh --zip           # also write the release zip
```

Set `MCPGW_SIGN_IDENTITY` to a Developer ID identity to sign it for
distribution. The bundled binary then gets the hardened runtime, with the
JIT entitlements wasmtime needs for Wasm servers
(`macos/Resources/mcpgw.entitlements`).

## What it does

When it opens, the app runs `mcpgw status` to find the daemon and starts it
with `mcpgw up` if it isn't running. You can turn that off in Settings. After
that it reads and changes everything through the admin API
(`/api/v1`), with the loopback token from `mcpgw token`. It refreshes every
two seconds.

| Section | What's there |
|---|---|
| Overview | daemon state, uptime, endpoints to copy, servers that failed or are quarantined |
| Servers | global servers (badge **global**), and the servers projects declare in their `.mcpgw.toml` (badge **project**, one row per name with the number of projects); state, kind and tools of each; enable, disable, restart, accept new tools, remove; details, pinned tools and live stderr. A project server's details list its **associated projects**, each with its own state, and show the picked project's copy |
| Projects | each project's `.mcpgw.toml`, what its servers run and which secrets they get; **Review and Trust…**, Untrust, Forget; Add Project creates a file from the template |
| Add Server | a form for `mcpgw add`: command, URL, container image, Wasm component, OpenAPI spec or MCP Registry entry, with env, headers, secrets and flags |
| Tools | every public tool, with an inspector that calls it (policy, approvals and audit apply). The **scope** picker in the toolbar chooses whose: **Global** (the default endpoint, global servers only) or a trusted project, which lists its servers' tools with the global ones and calls them through its endpoint, as a client started there would |
| Approvals | calls waiting for a human decision, with Approve and Deny |
| Activity | recent calls and their outcomes |
| Profiles | each profile's endpoint, servers, policy and the tools it sees |
| Secrets | vault metadata; set or delete values (they can't be read back) |
| Audit Log | the hash-chained log, with a verify button |
| Clients | whether Claude Desktop, Claude Code, Cursor and VS Code go through mcpgw (from `mcpgw doctor`), and Connect (`mcpgw client install`) |
| Doctor | `mcpgw doctor`'s findings and fixes |

**Server states**, as the Servers and Projects screens show them:

| Badge | Means |
|---|---|
| cold | not running; it starts on its next call (it also stops after `idle_timeout`, 10 minutes by default, without calls) |
| starting | launching and connecting; calls wait |
| ready / idle | running, with no call in progress |
| busy | running a call right now |
| draining | stopping; calls in progress finish first |
| backoff | its last start failed; it tries again shortly |
| failed | gave up after repeated failed starts; **Restart** clears it |
| quarantined | its tools changed since they were pinned; calls are refused until you **Accept New Tools** |
| disabled | turned off |
| off | a project server whose project isn't trusted |

The menu bar icon shows the daemon's state and any calls waiting for
approval, which you can approve or deny from there. It changes to an
alert icon while something waits. A notification is also sent for each new
approval, and the Dock icon shows how many are waiting.

**Gateway → Start / Stop / Restart** run `mcpgw up` and `mcpgw down`. The
daemon is a separate process, so it keeps serving clients after the app
quits, unless **Stop the gateway when the app quits** is on.

## Things to know

- **PATH.** Apps opened from Finder get a minimal `PATH`. The app reads your
  login shell's `PATH` (plus Homebrew's directories) and starts the daemon
  with it, so `npx`, `uvx` and `docker` servers work as they do from a
  terminal.
- **Clients run the bundled binary.** **Clients → Connect** writes the
  bundled `mcpgw` path into the client's config. Keep the app in one place
  (`/Applications`), or reconnect the clients after moving it.
- **Command-line tool.** **mcpgw → Install Command-Line Tool…** links
  `~/.local/bin/mcpgw` to the bundled binary, so terminals use the same
  version as the app.
- **Config.** Settings → Config file passes `--config` to every command.
  Leave it empty to use `$MCPGW_CONFIG` or `~/.config/mcpgw/config.toml`.
- **Keychain.** With a keychain-backed vault, macOS asks once whether the
  bundled `mcpgw` may use the vault key. After a rebuild it asks again,
  because an ad hoc signature changes with every build.
- **The web UI** is still there: **Gateway → Open Web UI**.

## Development

The Swift package is in `macos/`.

- **`McpgwKit`** holds the API client, the models and the CLI runner. It
  has no UI, so it can be tested.
- **`McpgwApp`** holds the SwiftUI app.

```sh
cargo build -p mcpgw -p mcpgw-testkit   # the binaries the tests drive
swift test --package-path macos         # unit tests + a real daemon end to end
MCPGW_BIN=target/debug/mcpgw swift run --package-path macos McpgwApp
```

The end-to-end test starts a daemon on a private config and checks every
endpoint the app reads, so a change to the API's JSON fails it. CI runs it
on macOS (`MCPGW_MAC_E2E=1` makes missing binaries an error).
