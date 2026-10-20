#!/usr/bin/env bash
# Write crates/mcpgw/tests/fixtures/m3/ with the M3 build (c8eee8c): a
# config, a store with a stored server (bound to a vault secret, its tools
# pinned) and a vault. tests/upgrade.rs runs v1.0 against a copy.
# Usage: make-m3-fixture.sh <m3 mcpgw binary> <fake-mcp binary> <out dir>
set -euo pipefail
m3=$1; fake=$2; out=$3
work=$(mktemp -d)
mkdir -p "$work/bin"
cp "$fake" "$work/bin/fake-mcp"
export PATH="$work/bin:$PATH" MCPGW_VAULT_PASSPHRASE=m3-fixture-passphrase
cat > "$work/config.toml" <<CFG
[gateway]
mode = "direct"
store_path = "@DIR@/db.sqlite"
token_path = "@DIR@/token"
listen = "127.0.0.1:0"

[[profile]]
name = "narrow"
servers = ["echo"]
mode = "direct"
policy = { deny = ["echo__crash"] }
CFG
sed "s|@DIR@|$work|g" "$work/config.toml" > "$work/live.toml"
mcpgw() { "$m3" --config "$work/live.toml" "$@"; }
printf 'm3-secret-value' | mcpgw secret set m3_secret
mcpgw add echo --secret FAKE_FROM_VAULT=m3_secret --command fake-mcp
mcpgw list
mcpgw --version > "$work/m3-version.txt"
mkdir -p "$out"
cp "$work/config.toml" "$work/db.sqlite" "$work/vault.json" "$work/m3-version.txt" "$out/"
rm -rf "$work"
echo "fixture written to $out"
