#!/usr/bin/env bash
# Build mcpgw.app: the SwiftUI app with the mcpgw binary bundled inside
# (Contents/MacOS/mcpgw), signed ad hoc unless MCPGW_SIGN_IDENTITY names a
# Developer ID identity.
#
#   macos/build.sh [--universal] [--debug] [--zip] [--out DIR]
#
# --universal builds arm64 + x86_64 (both Rust targets must be installed;
# this script never installs toolchains). The app lands in target/macos/.
# MCPGW_VERSION overrides the version (default: the workspace's).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
universal=0
profile=release
zip=0
out="$root/target/macos"
while [ $# -gt 0 ]; do
  case "$1" in
    --universal) universal=1 ;;
    --debug) profile=debug ;;
    --zip) zip=1 ;;
    --out) out=$2; shift ;;
    *) echo "usage: $0 [--universal] [--debug] [--zip] [--out DIR]" >&2; exit 2 ;;
  esac
  shift
done

version=${MCPGW_VERSION:-$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")}
build=$(git -C "$root" rev-list --count HEAD 2>/dev/null || echo 1)
bundle_id=${MCPGW_BUNDLE_ID:-io.github.abindran.mcpgw}
identity=${MCPGW_SIGN_IDENTITY:--}
cargo_flags=(--locked -p mcpgw)
[ "$profile" = release ] && cargo_flags+=(--release)

echo "==> mcpgw $version ($profile$([ $universal = 1 ] && echo ', universal'))"
work="$out/work"
rm -rf "$work"
mkdir -p "$work"

# 1. the Rust binary
if [ $universal = 1 ]; then
  sysroot=$(rustc --print sysroot)
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    [ -d "$sysroot/lib/rustlib/$t" ] || { echo "the $t Rust target isn't installed" >&2; exit 1; }
    (cd "$root" && cargo build "${cargo_flags[@]}" --target "$t")
  done
  lipo -create -output "$work/mcpgw" \
    "$root/target/aarch64-apple-darwin/$profile/mcpgw" "$root/target/x86_64-apple-darwin/$profile/mcpgw"
else
  (cd "$root" && cargo build "${cargo_flags[@]}")
  cp "$root/target/$profile/mcpgw" "$work/mcpgw"
fi

# 2. the Swift app
swift_flags=(-c "$profile" --package-path "$here")
[ $universal = 1 ] && swift_flags+=(--arch arm64 --arch x86_64)
swift build "${swift_flags[@]}" --product McpgwApp
app_bin="$(swift build "${swift_flags[@]}" --show-bin-path)/McpgwApp"

# 3. the icon
swift "$here/make-icon.swift" "$work/AppIcon.iconset"
iconutil -c icns -o "$work/AppIcon.icns" "$work/AppIcon.iconset"

# 4. the bundle
app="$out/mcpgw.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$app_bin" "$app/Contents/MacOS/McpgwApp"
cp "$work/mcpgw" "$app/Contents/MacOS/mcpgw"
cp "$work/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$app/Contents/Resources/"
sed -e "s/@VERSION@/$version/" -e "s/@BUILD@/$build/" -e "s/@BUNDLE_ID@/$bundle_id/" \
  "$here/Resources/Info.plist" > "$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"

# 5. signing: the helper first, then the app around it
if [ "$identity" = - ]; then
  codesign --force --sign - "$app/Contents/MacOS/mcpgw"
  codesign --force --sign - "$app"
else
  codesign --force --timestamp --options runtime --entitlements "$here/Resources/mcpgw.entitlements" \
    --sign "$identity" "$app/Contents/MacOS/mcpgw"
  codesign --force --timestamp --options runtime --sign "$identity" "$app"
fi
codesign --verify --strict --deep "$app"
rm -rf "$work"

if [ $zip = 1 ]; then
  arch=$([ $universal = 1 ] && echo universal || uname -m)
  (cd "$out" && ditto -c -k --keepParent mcpgw.app "mcpgw-$version-macos-$arch-app.zip")
  echo "==> $out/mcpgw-$version-macos-$arch-app.zip"
fi
echo "==> $app"
"$app/Contents/MacOS/mcpgw" --version
