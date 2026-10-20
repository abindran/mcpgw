# M2-13 — Hash-chained audit log

**Story:** X · **Estimate:** 2 days · **Depends on:** M1-03
**Done when:** tampering with any row breaks chain verification.
**Status:** ✅ Done

## What is recorded

| Kind | Actor | Subject | Detail |
|------|-------|---------|--------|
| `tool.call` | endpoint / client (`profile web / Cursor`) | public tool | outcome (`ok`, `tool_error`, `error`), duration; **`args_digest`** = SHA-256 of the canonical JSON of the arguments |
| `approval.decide` | endpoint | tool | approval id, approved; `args_digest` of the call |
| `server.add` · `.update` · `.remove` · `.enable` · `.disable` · `.pin` | `cli` or `api` | server | transport, pin |
| `profile.put` · `.delete` | `api` | profile | servers |
| `secret.set` · `.delete` | `cli` or `api` | secret **name** | — |
| `auth.login` | `browser` | `provider@account` | — |

Arguments, results and secret values are **never** stored; the digest
proves which call was made without keeping the data. Each change is recorded
once: CLI commands write their own entry, and the daemon's internal
`/admin` routes they use don't add a second one. Writes never block the
action; a failure is logged.

## Chain (`mcpgw-core::audit`, `mcpgw-store` migration 3)

- Table `audit(seq, at_ms, kind, actor, subject, args_digest, detail,
  prev_hash, hash)`. `seq` counts from 1; the first `prev_hash` is 64 zeros.
- `hash = SHA-256(prev_hash ‖ "\n" ‖ canonical JSON {seq, at_ms, kind,
  actor, subject, args_digest, detail})`.
- **Append-only in SQL**: `BEFORE UPDATE` and `BEFORE DELETE` triggers abort
  with "the audit log is append-only".
- **Verification** (`ChainVerifier`, run page by page) checks that sequence
  numbers are consecutive from 1, each `prev_hash` is the previous `hash`,
  and each `hash` matches the content. It reports the first broken entry and
  why: missing or out of order, doesn't link, or modified.

Someone with the database file can drop the triggers, but can't edit
anything without verification noticing. Rewriting one row's hash breaks the
next link. Rewriting every hash from that point on would pass the chain
check; anchoring the head externally (exporting it to a log server or
signing it) is future work in the plan.

## Using it

```sh
mcpgw audit verify               # "audit log OK: 7 entries, chain intact", or exit 1 with the break
mcpgw audit export [--after N]   # JSON lines on stdout
mcpgw audit tail [-n 20]
```

API: `GET /api/v1/audit?after=&limit=` and `GET /api/v1/audit/verify`
(in the OpenAPI document and the API test).

## Tests

- Store: append and link; triggers block `UPDATE` and `DELETE`; with the
  triggers dropped, editing `subject`, `args_digest`, `detail`, `at_ms`,
  `actor` or `hash`, deleting the first or a middle row, or appending a
  forged row is caught **at the right entry**. Rewriting a row together with
  its own hash is caught at the next entry.
- Core: verify / edit / delete / swap / truncate; digests ignore key order.
- End to end (`crates/mcpgw/tests/audit.rs`): `secret set`, `add`,
  `disable`, `enable` (CLI, daemon running) and three tool calls produce
  exactly seven entries in order with the right actors. The `fx__add` digest
  equals the digest of `{"a":1,"b":2}`, the echoed text and the secret value
  appear nowhere, `export --after 5` gives 2 lines, and `tail -n 2` shows the
  last two. Then the test edits a row in the SQLite file: a plain UPDATE is
  refused; after dropping the trigger it succeeds, and `mcpgw audit verify`
  exits 1 with "audit chain broken at entry 6: content does not match its
  hash".
- API test: the API's server, profile and secret changes, approval decisions
  and tool calls are in `/api/v1/audit`, and `/api/v1/audit/verify` is ok.
