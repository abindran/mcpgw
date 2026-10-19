#!/usr/bin/env bash
# Proves the supply-chain gate bites: cargo-deny must reject a crate with a
# known advisory and a GPL dependency (fixtures in testdata/supply-chain),
# each with the expected diagnostic rather than an unrelated failure.
set -euo pipefail
cd "$(dirname "$0")/.."
deny=${CARGO_DENY:-cargo-deny}

expect_reject() {
  local fixture=$1 check=$2 diagnostic=$3 out
  if out=$("$deny" --manifest-path "testdata/supply-chain/$fixture/Cargo.toml" \
      --config deny.toml check "$check" 2>&1); then
    echo "::error::cargo-deny $check accepted testdata/supply-chain/$fixture"
    exit 1
  fi
  if ! grep -q "$diagnostic" <<<"$out"; then
    echo "::error::cargo-deny $check failed on $fixture without '$diagnostic':"
    echo "$out"
    exit 1
  fi
  echo "ok: cargo-deny $check rejects $fixture ($diagnostic)"
}

expect_reject vulnerable advisories 'error\[vulnerability\]'
expect_reject gpl licenses 'error\[rejected\]'
