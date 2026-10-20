#!/usr/bin/env bash
# Usage: homebrew-formula.sh <version> <dist-dir>. Print a Homebrew formula
# for the release archives in <dist-dir> (with their SHA-256s), for a tap.
set -euo pipefail
version=$1; dist=$2
repo=${GITHUB_REPOSITORY:-abindran/mcpgw}
sha() { sha256sum "$dist/mcpgw-$version-$1.tar.gz" | cut -d' ' -f1; }
url() { echo "https://github.com/$repo/releases/download/v$version/mcpgw-$version-$1.tar.gz"; }
cat <<RB
class Mcpgw < Formula
  desc "Local MCP gateway: one config for every MCP server"
  homepage "https://github.com/$repo"
  version "$version"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "$(url aarch64-apple-darwin)"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$(url x86_64-apple-darwin)"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end
  on_linux do
    url "$(url x86_64-unknown-linux-musl)"
    sha256 "$(sha x86_64-unknown-linux-musl)"
  end

  def install
    bin.install "mcpgw"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/mcpgw --version")
  end
end
RB
