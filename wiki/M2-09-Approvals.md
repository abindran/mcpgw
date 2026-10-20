# M2-09 — Approvals

**Story:** US3 · **Estimate:** 4 days · **Depends on:** M2-08
**Done when:** `merge_pull_request` waits for approval in at least two clients.
**Status:** ✅ Done (verified with rmcp clients speaking 2026-07-28 and 2025-11-25; Claude Code and Cursor runs need a person)

## Config

```toml
[gateway]
policy = { approve = ["github__merge_*"] }   # or per [[profile]]
approval_timeout = "2m"                      # CLI fallback wait (default 2m)
```

## How a call is approved

When the policy says **approve** for the tool being run (by name, or the
target of `call_tool`):

| Client | Mechanism |
|--------|-----------|
| 2026-07-28 with form elicitation | **MRTR** (SEP-2322): the call returns an `InputRequiredResult` with one `elicitation/create` form (`approve: boolean`) and `requestState` = the approval id; the client asks the user and **retries** with `inputResponses` |
| 2025-11-25 with form elicitation | **`elicitation/create`** on the client's session; the gateway waits (up to 5 min) and continues the same call |
| anything else, or nobody answers | **CLI fallback**: the call waits up to `approval_timeout` for `mcpgw approve <id>` / `mcpgw deny <id>` (`mcpgw approvals` lists them) |

The question reads: "Allow github__merge_pull_request on profile frontend?
Arguments: {…} (or run `mcpgw approve <id>`)".

MRTR is used only when the client declares form elicitation. A 2026 client
that can't answer falls back to the CLI instead of getting a question it
must decline.

## Safety (`mcpgw-runtime::approvals`)

- Each approval is bound to a **SHA-256 of the tool name and its exact
  arguments** (canonical JSON). MRTR's `requestState` comes back from the
  client and is untrusted, so it is only an opaque id: approving one call
  can't be reused for another call or other arguments.
- **Single use**: redeeming removes it. Undecided approvals expire after 10
  minutes. A decision can't be changed.
- A refusal never reaches the server: "Tool X was not run: X was not
  approved." A timeout explains how to approve next time.

## Plumbing

- `mcpgw-proto`: `ProtoError::Elicit { state, message, schema }` lets the
  provider ask a question; `CallContext` gains `mrtr`, `form_elicitation` and
  `input` (the answer). The front end turns `Elicit` into MRTR or a session
  elicitation and calls the provider again with the answer. rmcp's
  `elicitation` feature is enabled.
- Admin API: `GET /admin/approvals`, `POST /admin/approvals/{id}/approve`
  and `/deny`. CLI: `mcpgw approvals`, `mcpgw approve <id>`,
  `mcpgw deny <id>`.
- testkit: `McpClient::http_kind(url, token, ClientKind { approve, .. },
  legacy)` declares form elicitation, answers like a user, and records what
  it was asked. fake-mcp gains `FAKE_CALL_LOG`.

## Tests (`crates/mcpgw/tests/approvals.rs`)

- `merge_waits_for_approval_in_2026_and_2025_clients`: a **2026-07-28 client
  (MRTR)** and a **2025-11-25 client (session elicitation)** each get one
  question naming the tool, endpoint and arguments; on "approve" the merge
  runs (call log: 1, then 2). On "deny" (both protocols) the result is
  "was not approved" and the call log is unchanged. `list_prs`, not on the
  approve list, runs without a question.
- `cli_fallback_approves_and_denies`: a client without elicitation waits;
  `mcpgw approvals` shows the pending call with endpoint and arguments;
  `mcpgw approve <id>` lets it run; `mcpgw deny <id>` refuses the next one;
  an unknown id is an error.
- Units: binding to the exact call (argument key order doesn't matter, the
  content does), single use, pending vs denied, waiters wake, timeouts.
