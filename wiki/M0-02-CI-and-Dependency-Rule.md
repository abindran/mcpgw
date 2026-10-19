# M0-02 — CI pipeline + dependency-rule enforcement

**Status:** ✅ Implemented (CI proof run pending runner availability)

## What was added

| File | Purpose |
|------|---------|
| `.github/workflows/ci.yml` | Two jobs: `rules` and `check` |
| `scripts/check-deps.sh` | Fails if any crate except `mcpgw-proto` references `rmcp` |
| `scripts/canary-scan.sh` | Fails if a canary secret (`mcpgw-canary-<16+ alnum>`) is tracked outside `scripts/` and `tests/fixtures/` |

## CI jobs

- **rules** — runs `check-deps.sh` and `canary-scan.sh`.
- **check** — `cargo fmt --check`, `cargo clippy -D warnings`, `cargo build`, `cargo test` (all workspace).

## Dependency rule check

Two layers:
1. Source/manifest grep of every `crates/*` except `mcpgw-proto` (comment-only lines ignored).
2. `cargo metadata` — fails if any non-proto package declares an `rmcp` dependency.

## Verification

- Local: clean tree passes; appending `use rmcp::x;` to `mcpgw-core` fails with exit 1 and a clear error; fmt, clippy, tests pass.
- Remote: PR #1 (`bad-rmcp`, "DO NOT MERGE") carries the deliberate violation and should show the `rules` job failing. Close it without merging once confirmed.

## Notes

- Canary scan is a simple pattern check; richer scanning is planned later.
- Add branch protection requiring both jobs once CI is green.
