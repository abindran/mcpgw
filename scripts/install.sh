#!/bin/sh
# Install mcpgw from a GitHub release: picks the archive for this machine,
# checks its SHA-256 against the release's SHA256SUMS, and puts the binary in
# $MCPGW_INSTALL_DIR (default ~/.local/bin).
#   curl -fsSL https://github.com/abindran/mcpgw/releases/latest/download/install.sh | sh
#   MCPGW_VERSION=1.0.0 sh install.sh
set -eu
repo="${MCPGW_REPO:-abindran/mcpgw}"
dir="${MCPGW_INSTALL_DIR:-$HOME/.local/bin}"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  *) echo "mcpgw: no release build for $(uname -s) $(uname -m); build it with cargo install" >&2; exit 1 ;;
esac
if [ -z "${MCPGW_VERSION:-}" ]; then
  MCPGW_VERSION=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n1)
fi
name="mcpgw-$MCPGW_VERSION-$target"
base="https://github.com/$repo/releases/download/v$MCPGW_VERSION"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$base/$name.tar.gz" -o "$tmp/$name.tar.gz"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
(cd "$tmp" && grep " $name.tar.gz\$" SHA256SUMS | { if command -v sha256sum >/dev/null; then sha256sum -c -; else shasum -a 256 -c -; fi; }) >/dev/null
tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$dir"
install -m 0755 "$tmp/$name/mcpgw" "$dir/mcpgw"
echo "installed mcpgw $MCPGW_VERSION to $dir/mcpgw"
case ":$PATH:" in *":$dir:"*) ;; *) echo "add $dir to your PATH" ;; esac
