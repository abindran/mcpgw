# Developing a server

`mcpgw dev` runs the server you are working on under a private gateway.
Every edit restarts the server, and the next call runs the new code.

```sh
mcpgw dev ./my-server                    # package.json → node, server.py / main.py → python3, Cargo.toml → cargo run
mcpgw dev ./tool.wasm                    # a Wasm component: reloaded on every rebuild
mcpgw dev ./api.yaml --base-url http://localhost:8080   # an OpenAPI spec
mcpgw dev ./srv -- deno run --allow-net main.ts         # anything else: say how to run it
```

```
mcpgw dev: serving my-server at http://127.0.0.1:52311/mcp
inspector: http://127.0.0.1:52311/ui#token=…
watching /home/me/my-server (Ctrl-C to stop)
mcpgw dev: my-server ready, 1 tool(s)
12:03:41  my-server__hello {}  ->  ok 2 ms  hello v1
mcpgw dev: server.py changed; restarting my-server
mcpgw dev: my-server ready, 2 tool(s); added bye
```

- **Private.** The session has its own store, token and port, and leaves your
  servers and a running daemon alone. Your vault and gateway settings still
  apply.
- **Hot reload.**
  - The server's directory or file is watched, plus each `--watch`. Build
    output (`target`, `node_modules`, `__pycache__`, `.venv`, `.git`) is
    skipped.
  - Changed tools are accepted and the change is printed.
  - An edit that breaks the start is reported, and the next save tries
    again.
- **Request log.** Every call is printed with its arguments, outcome, time
  and the start of its result.
- **Inspector.** Open the printed URL. The web UI's Inspector lists every
  tool with a box for its arguments, and **Call** runs it. The Inspector is
  in the normal web UI too (`mcpgw ui`), and calls as the default endpoint
  does.

For a Wasm tool, the [template](wasm.md) and `mcpgw dev` together give a
build-and-try loop of a few seconds.
