# M0-04 — Protocol spike: forward one `tools/call`

**Story:** US1 · **Estimate:** 4 days · **Depends on:** M0-01
**Done when:** Claude Code calls a tool on the reference "everything" server through the spike.
**Status:** ✅ Done

The spike itself is described in [history/M0-04](history/M0-04-rmcp-Server-Spike.md)
and its findings in [ADR-0001](../docs/adr/0001-protocol.md). Since then the
spike has become the real stdio gateway (`mcpgw serve`).

## Verification against the reference server

`@modelcontextprotocol/server-everything` (TypeScript SDK, protocol
2025-11-25) behind `mcpgw serve`, driven by an MCP client over stdio:

```toml
[[server]]
name = "everything"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-everything"]
```

- `tools/list` returned the server's 13 tools as `everything__echo`,
  `everything__get-sum`, …
- `tools/call everything__echo {"message": "hello through mcpgw"}` returned
  `Echo: hello through mcpgw`.

To repeat it in Claude Code, add `{"mcpServers": {"mcpgw": {"command":
"mcpgw", "args": ["serve"]}}}` (or `claude mcp add mcpgw -- mcpgw serve`) and
ask it to call `everything__echo`. The recorded demo for the M0 exit is still
to be done by a person.

## Bug found and fixed: slow first start

The first run failed: `connect failed: uncorrelated error response: expected
id 1, error response carried 0`. A cold `npx -y` download takes longer than
rmcp's fixed 10-second `server/discover` probe. rmcp then sends
`initialize` (id 1), and the server's late reply to the abandoned probe
(id 0) aborts the handshake. The supervisor's restart recovered on the
second spawn, but the first `tools/list` came back empty.

**Fix (`mcpgw-proto`):** `BackendConn::connect_stdio` retries once with a
plain legacy `initialize` when the automatic handshake fails for any reason
other than a spawn error, within the same connect deadline.

**Regression test:** `slow_first_start_of_legacy_server_connects` starts a
legacy fake that waits 10.5 s on its first start only (`FAKE_DELAY_MARKER`).
It requires the state to go straight from `Starting` to `Running`. Without
the fix it goes `Starting → Restarting("uncorrelated error response …") →
Running` and fails.

## Also

`.gitattributes` forces LF line endings. Windows checkouts used CRLF, which
broke the byte-for-byte schema check (M0-06).
