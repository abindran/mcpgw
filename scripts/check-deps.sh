#!/usr/bin/env bash
# Fails if any crate other than mcpgw-proto references `rmcp`.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
for dir in crates/*/; do
  name=$(basename "$dir")
  [ "$name" = "mcpgw-proto" ] && continue
  # Ignore comment-only lines (//, //!, ///, #).
  if hits=$(grep -rnE '\brmcp\b' "$dir" --include='*.rs' --include='Cargo.toml' 2>/dev/null \
    | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|#)'); then
    echo "::error::rmcp referenced outside mcpgw-proto (crate: $name)"
    echo "$hits"
    fail=1
  fi
done

# Workspace-level dependency table may declare rmcp, but only mcpgw-proto may use it.
if command -v cargo >/dev/null; then
  bad=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | python3 -c '
import json,sys
m=json.load(sys.stdin)
for p in m["packages"]:
    if p["name"]!="mcpgw-proto" and any(d["name"]=="rmcp" for d in p["dependencies"]):
        print(p["name"])')
  if [ -n "$bad" ]; then
    echo "::error::Cargo dependency on rmcp in: $bad"
    fail=1
  fi
fi

[ $fail -eq 0 ] && echo "dependency rule OK: rmcp confined to mcpgw-proto"
exit $fail
