# M4-10 — `mcpgw dev`: hot reload, request log, inspector

**Story:** US1 · **Estimate:** 4 days · **Depends on:** M2-12
**Done when:** editing a local server restarts it and the next call uses the new code.
**Status:** ✅ Done: an edited server is restarted and the next call returns the new reply, with new and removed tools reported; a broken edit is reported and the session carries on; a replaced `.wasm` component is reloaded.

## Using it

```sh
mcpgw dev ./my-server                    # package.json → node <bin|main>, server.py / main.py → python3, Cargo.toml → cargo run
mcpgw dev ./tool.wasm                    # a Wasm component (M4-01)
mcpgw dev ./api.yaml --base-url http://localhost:8080   # an OpenAPI spec (M4-05)
mcpgw dev ./srv -- deno run --allow-net main.ts         # anything else: say how to run it
mcpgw dev ./srv --env API_KEY=test --watch ../shared --port 7800
```

```
mcpgw dev: serving my-server at http://127.0.0.1:52311/mcp
inspector: http://127.0.0.1:52311/ui#token=…
watching /home/me/my-server (Ctrl-C to stop)
mcpgw dev: my-server ready, 1 tool(s)
12:03:41  my-server__hello {}  ->  ok 2 ms  hello v1
mcpgw dev: tools.json changed; restarting my-server
mcpgw dev: my-server ready, 2 tool(s); added bye
```

Point a client (or the inspector) at the printed URL with the printed
token. Ctrl-C ends the session.

## What it does

- **A private gateway.** The session's store, token and daemon file live
  in a temporary directory, removed on exit.
  - `mcpgw dev` never touches your servers, profiles or a running daemon,
    which can keep running next to it.
  - The config's gateway settings apply (vault, timeouts, OAuth apps), so
    `${secret:…}` and logins work as usual.
  - Policy is cleared, and every tool is listed directly.
- **Hot reload.**
  - The watched paths are polled every 300 ms: the server's directory or
    file, plus each `--watch`.
  - `.git`, `target`, `node_modules`, `__pycache__`, `.venv` and
    `.mypy_cache` are skipped, so builds don't cause restarts.
  - After a change the files must be still for 300 ms. The server is then
    stopped and started again, and the next call runs the new code.
  - `cargo run` rebuilds on start; the start may take up to 5 minutes.
- **Changed tools are accepted.**
  - In normal use a server whose tools change is quarantined (M1-07).
  - In dev the new tools are pinned (`approve_tools`) and the change is
    printed: added and removed tools.
- **Broken edits.**
  - When the new code fails to start, the error is printed ("failed to
    start: …") and the watch goes on.
  - The next save tries again.
- **Request log.** One line per call on stderr:
  - time, tool and arguments;
  - outcome (`ok`, `tool_error`, `error`) and milliseconds;
  - the first 160 characters of the result's text, or of the error.

  It comes from the router's new `CallTap`, which sees every finished call.
- **Inspector.** The web UI (M2-12) has an **Inspector** section, in dev and
  in normal use:
  - every tool, with its description and a JSON box pre-filled from its
    input schema;
  - **Call** runs it through the new `POST /api/v1/tools/{name}/call` and
    shows the result.

  It calls as the default endpoint does: policy, approvals and audit
  apply, and approvals appear in the same page.

## Code

- `crates/mcpgw/src/dev.rs`:
  - `spec` detects how to run the target;
  - `config` builds the private session;
  - `snapshot`/`changed`/`watch` do the hot reload;
  - `start` restarts and reports, accepting changed tools;
  - `tap` is the request log;
  - `run` ties it together.
- `serve::serve_with(…, Some(Dev))`:
  - the watcher starts the server, instead of the start-up capture;
  - the connect timeout is longer;
  - the banner is printed;
  - the tap is set.
- `mcpgw-router`: `GatewayOptions::tap` (`CallTap`).
- `mcpgw-api`:
  - `ApiState::invoke` (`Invoke`) and `POST /api/v1/tools/{name}/call`,
    which is in the OpenAPI document;
  - the UI's Inspector section (`app.js`, `index.html`, `app.css`).
- The serve path sets `invoke` from a `GatewayProvider` with the default
  endpoint's options.
- `fake-mcp`: `FAKE_TOOLS_FILE` reads its tools from a file at start, so
  editing the file is editing the server.

## Tests

- `crates/mcpgw/tests/dev.rs` (end to end):
  - **Edit:** a server directory runs with `-- fake-mcp`, and its tools
    come from `tools.json`.
    - The first call gives "hello v1".
    - Rewriting the file, as editors do (write, then rename), gives
      "changed; restarting" and "ready, 2 tool(s); added bye".
    - The next call returns "hello v2", and `bye` is listed.
  - **Broken edit:** the error is reported. A fixed edit gives "removed
    bye", and the call returns "hello v3".
  - **Request log:** lines show the tool, arguments and outcome.
  - **Inspector:** `POST /api/v1/tools/my-server__hello/call` returns the
    result, and `/ui` has the Inspector.
  - **Wasm:** replacing `tool.wasm` with another component reloads it, and
    the tool list changes.
- `crates/mcpgw/tests/api.rs`: the new endpoint is exercised like every
  other: a call returns the result, and an unknown tool is a 502 that names
  it.
- `dev.rs` unit tests:
  - detection: python, node `main`, an explicit command;
  - names from paths, `.wasm` and OpenAPI files, other files refused;
  - snapshots see edits but not `target/`.
- UI: the page uses only documented endpoints (now `/tools` and `/call`
  too); `app.js` passes `node --check`.
