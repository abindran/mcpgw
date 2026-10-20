# M1-09 — Aggregating router and namespacing

**Story:** US1, US3 · **Estimate:** 3 days · **Depends on:** M1-07
**Done when:** two servers with a tool named `search` both work without collision.
**Status:** ✅ Done

## Public tool names (`mcpgw-router::aggregate`)

`<prefix>__<tool>`, where the prefix is the server's **alias** (optional
`alias` field, `add --alias`), else its name.

Strict clients (Cursor, OpenAI-compatible hosts) accept at most 64
characters of `[A-Za-z0-9_-]`, so:

- If `prefix__tool` fits and uses only allowed characters, it is used as
  is: `github__search`.
- Otherwise disallowed characters become `_`, the name is cut to 55
  characters, and `_` plus the first 8 hex digits of the SHA-256 of the
  *original* `prefix__tool` is appended: exactly 64 characters, stable across
  restarts, and distinct for names sharing a 55-character start.
- A sanitised name always gets a hash, so `read.file` and `read_file` don't
  collide.
- Descriptions are tagged `[prefix] …`.

## Routing

`resolve(public)` splits at the first `__` (prefixes can't contain `__`),
maps the prefix to its server, then matches against that server's pinned
manifest, so hashed names route back without a stored mapping. A tool not in
the manifest passes through by its plain name.

Prefixes must be unique: the config loader rejects a clash (after checking
duplicate names, so "duplicate server name" is reported first), `add` refuses
one, and the runtime falls back to the server name if stored and config
servers clash.

## Tests

- Unit: plain names; 64-character truncation with a stable hash; different
  tails give different hashes; exactly 64 stays plain; sanitising adds a hash;
  same-named tools on two servers.
- Integration (`mcpgw-router/tests/gateway.rs`): `github` and
  `gitlab-enterprise` (alias `gl`), both with `search`: listing has
  `github__search` and `gl__search`, every name ≤ 64 characters, and both
  calls reach their own server; the hashed long name and the sanitised
  `read.file` route correctly; the unaliased name is not a prefix.

(This page was missing from the original M1-09 commit and was added with
M2-03.)
