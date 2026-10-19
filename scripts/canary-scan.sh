#!/usr/bin/env bash
# Fails if a canary secret (mcpgw-canary-<16+ chars>) is committed outside tests/fixtures.
set -euo pipefail
cd "$(dirname "$0")/.."

pattern='mcpgw-canary-[A-Za-z0-9]{16,}'
if hits=$(git ls-files -z \
  | xargs -0 grep -nE "$pattern" 2>/dev/null \
  | grep -vE '^(scripts/|tests/fixtures/|crates/[^/]+/tests/fixtures/)'); then
  echo "::error::Canary secret found in tracked files"
  echo "$hits"
  exit 1
fi
echo "canary scan OK"
