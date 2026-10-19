# M0-02 — CI pipeline and build matrix

**Story:** X · **Estimate:** 2 days · **Depends on:** M0-01
**Done when:** every PR runs the full matrix in under 15 minutes.
**Status:** ✅ Done

## Jobs (`.github/workflows/ci.yml`)

| Job | Runner | What it does |
|-----|--------|--------------|
| `rules` | ubuntu | `scripts/check-deps.sh` (rmcp only in `mcpgw-proto`) and `scripts/canary-scan.sh` |
| `lint` | ubuntu | `cargo fmt --check`, `cargo clippy -D warnings` |
| `msrv` | ubuntu | `cargo check` on Rust 1.88 |
| `test (macOS arm64)` | macos-14 | build + test, `aarch64-apple-darwin` |
| `test (macOS x86_64)` | macos-15-intel | build + test, `x86_64-apple-darwin` |
| `test (Linux glibc)` | ubuntu | build + test, `x86_64-unknown-linux-gnu` |
| `test (Linux musl)` | ubuntu + musl-tools | build + test, `x86_64-unknown-linux-musl` |
| `test (Windows)` | windows-latest | build + test, `x86_64-pc-windows-msvc` |

- Each test job has a 15-minute timeout and uploads the `mcpgw` binary as an
  artifact (kept 7 days).
- `--locked` everywhere: `Cargo.lock` is now committed (it was ignored), so CI
  builds exactly the reviewed dependency set.
- A newer push cancels the running workflow for the same branch.
- `cargo xtask ci` runs the same gates locally.

## Notes

- Tests no longer need Python (M0-10), so the same suite runs on all five
  targets.
- The CLI test no longer assumes a Unix binary name (`fake-mcp.exe` on Windows).
