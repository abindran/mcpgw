#!/usr/bin/env bash
# Run all local gates, then commit and push.  Usage: scripts/ship.sh "message"
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -q -- -D warnings
cargo test --workspace -q 2>&1 | grep -E "test result|FAILED|panicked" | sort | uniq -c
bash scripts/check-deps.sh
bash scripts/canary-scan.sh >/dev/null
git add -A
git commit -qm "$1"
git push -q 2>&1 | tail -2
git log --oneline -1
