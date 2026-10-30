#!/usr/bin/env bash
# Assemble OpenCrabs.app from a built binary.
#
# A Mach-O executable has no icon field. macOS reads an app icon from exactly
# one place: a bundle's Contents/Resources/icon.icns, named by CFBundleIconFile
# in Contents/Info.plist. Shipping a bare binary therefore ships the generic
# exec glyph, which is what the tarballs did before this script existed.
#
# The bundle is deliberately left unsigned. The binary already carries a
# linker-signed adhoc signature, and /evolve replaces Contents/MacOS/opencrabs
# in place, which would invalidate a bundle signature. A stale _CodeSignature
# is worse than none.
#
# Usage:
#   scripts/package-macos-app.sh \
#     --binary target/aarch64-apple-darwin/release/opencrabs \
#     --rtk    target/aarch64-apple-darwin/release/rtk \
#     --version 0.5.11 \
#     --out    dist
#
# Writes dist/OpenCrabs.app.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ICNS="$REPO_ROOT/src/assets/icon.icns"

BINARY=""
RTK=""
VERSION=""
OUT=""

die() {
    echo "package-macos-app: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --binary)  BINARY="${2:-}";  shift 2 ;;
        --rtk)     RTK="${2:-}";     shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --out)     OUT="${2:-}";     shift 2 ;;
        -h|--help)
            sed -n '2,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -n "$BINARY" ]  || die "--binary is required"
[ -n "$RTK" ]     || die "--rtk is required"
[ -n "$VERSION" ] || die "--version is required"
[ -n "$OUT" ]     || die "--out is required"

[ -f "$BINARY" ] || die "binary not found: $BINARY"
[ -f "$RTK" ]    || die "rtk not found: $RTK"
[ -f "$ICNS" ]   || die "icon not found: $ICNS"

APP="$OUT/OpenCrabs.app"
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

rm -rf "$APP"
mkdir -p "$MACOS" "$RESOURCES"

# rtk must sit beside the binary: evolve resolves it as a sibling of exe_path.
install -m 0755 "$BINARY" "$MACOS/opencrabs"
install -m 0755 "$RTK" "$MACOS/rtk"
install -m 0644 "$ICNS" "$RESOURCES/icon.icns"

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>OpenCrabs</string>
    <key>CFBundleDisplayName</key>
    <string>OpenCrabs</string>
    <key>CFBundleIdentifier</key>
    <string>com.opencrabs.app</string>
    <key>CFBundleExecutable</key>
    <string>opencrabs</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

plutil -lint "$CONTENTS/Info.plist" > /dev/null

echo "built $APP (version $VERSION)"
