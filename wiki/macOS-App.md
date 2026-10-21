# macOS app (after M4)

**Not a plan item.** It was asked for after v1.0 was prepared: a native
macOS UI with the mcpgw binary bundled in the app.

## What was built

- **`macos/`**, a Swift package (Swift 6, macOS 14+):
  - **`McpgwKit`** (no UI):
    - `APIClient`: every `/api/v1` endpoint;
    - the models, with explicit snake_case `CodingKeys`, because
      `.convertFromSnakeCase` would also rewrite keys inside tool schemas;
    - `MCPGW`, the CLI runner: `status`, `token`, `up`, `down`, `add`,
      `client install`, `doctor --json`;
    - the Add Server form → `mcpgw add` arguments, with shell-style word
      splitting.
  - **`McpgwApp`** (SwiftUI): a main window with Overview, Servers (with
    Add Server; global servers, and project servers with their associated
    projects), Tools with an inspector and a scope picker (global or a project), Approvals, Activity, Profiles,
    Secrets, Audit Log, Clients and Doctor. Also a menu bar item with
    approvals, notifications, a Dock badge and Settings.
- **`macos/build.sh`** builds `mcpgw.app`:
  - builds the Rust binary (native or `--universal`) and the Swift app;
  - draws the icon (`make-icon.swift` → iconutil);
  - puts `mcpgw` in `Contents/MacOS/` next to the app's executable;
  - signs ad hoc, or with `MCPGW_SIGN_IDENTITY`, which adds the hardened
    runtime and wasmtime's JIT entitlements.
- **CI:** a `macOS app` job runs `swift test` with `MCPGW_MAC_E2E=1`,
  then builds the bundle.
- **Release:** a `macos-app` job adds
  `mcpgw-<version>-macos-universal-app.zip`, which is checksummed and
  signed with the other files.
- **Docs:** [docs/guides/macos-app.md](../docs/guides/macos-app.md).

## Design choices

- **Lifecycle through the CLI, everything else through the API.**
  `mcpgw status` honours the config's store path, which decides where
  `daemon.json` is, so the app never parses the TOML itself. Adding a
  server goes through `mcpgw add` rather than `POST /servers`, so it also
  gets introspection, pinning, the poisoning scan, the package cache and
  moving credentials to the vault.
- **PATH.** A Finder-launched app has a minimal `PATH`, so the app reads
  the login shell's `PATH` and starts the daemon with it. Otherwise `npx`
  and `uvx` servers would fail.
- **The daemon outlives the app**, as with the CLI. A daemon started
  elsewhere is picked up within 5 s, and a lost connection triggers a
  reconnect.

## Tests

- **Unit tests:** status parsing, word splitting, the add arguments, sample
  arguments from a schema, key preservation, call results, PATH merging.
- **End to end (`drivesARealDaemon`):**
  - on a private config, from `status` = stopped through `add` and `up`;
  - every read endpoint is decoded: runtime, servers, tools (also through a
    profile), the server's tools, call, calls, logs, server detail,
    enable/disable, restart, secrets set/list/delete, profiles, approvals,
    audit verify and list, a 404, doctor;
  - then `down`.
- **Checked by hand** with the built bundle on a throwaway config:
  - auto-start;
  - a pending approval made through `mcpgw connect`, shown in the app and
    approved (the client got `42`);
  - the Servers, Tools/inspector, Activity, Audit and Doctor screens.
