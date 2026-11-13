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
#     --out    dist \
#     [--gui   target/aarch64-apple-darwin/release/opencrabs-gui]
#
# Writes dist/OpenCrabs.app, and dist/OpenCrabs Desktop.app when --gui is
# given.
#
# --gui is optional so the script keeps working for a TUI-only bundle. When it
# is given, the desktop client lands in Contents/MacOS/ beside the terminal
# binary AND gets a bundle of its own. CFBundleExecutable in OpenCrabs.app
# stays `opencrabs`: /evolve replaces Contents/MacOS/opencrabs in place, and
# moving that name would break it. Double-clicking that bundle therefore opens
# the terminal client, which is why the window needs the second bundle, whose
# CFBundleExecutable is `opencrabs-gui`.
#
# The GUI binary is hard linked into the second bundle instead of copied, so
# the tarball carries it once and each bundle still works when extracted or
# copied on its own. A later /evolve rewrite of one path breaks the link and
# leaves the other holding the previous file, so evolve refreshes both.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ICNS="$REPO_ROOT/src/assets/icon.icns"

BINARY=""
GUI=""
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
        --gui)     GUI="${2:-}";     shift 2 ;;
        --rtk)     RTK="${2:-}";     shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --out)     OUT="${2:-}";     shift 2 ;;
        -h|--help)
            sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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
if [ -n "$GUI" ]; then
    [ -f "$GUI" ] || die "gui binary not found: $GUI"
fi
[ -f "$RTK" ]    || die "rtk not found: $RTK"
[ -f "$ICNS" ]   || die "icon not found: $ICNS"

APP="$OUT/OpenCrabs.app"
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

DESKTOP_APP="$OUT/OpenCrabs Desktop.app"

rm -rf "$APP"
mkdir -p "$MACOS" "$RESOURCES"

# rtk must sit beside the binary: evolve resolves it as a sibling of exe_path.
install -m 0755 "$BINARY" "$MACOS/opencrabs"
if [ -n "$GUI" ]; then
    install -m 0755 "$GUI" "$MACOS/opencrabs-gui"
fi
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

# The window gets a bundle of its own. OpenCrabs.app must keep
# CFBundleExecutable=opencrabs for /evolve, so double-clicking it opens the
# terminal client; without this second bundle the desktop client is reachable
# only from a shell.
if [ -n "$GUI" ]; then
    DESKTOP_CONTENTS="$DESKTOP_APP/Contents"
    DESKTOP_MACOS="$DESKTOP_CONTENTS/MacOS"
    DESKTOP_RESOURCES="$DESKTOP_CONTENTS/Resources"

    rm -rf "$DESKTOP_APP"
    mkdir -p "$DESKTOP_MACOS" "$DESKTOP_RESOURCES"

    # Hard links, not copies: tar stores the second occurrence of an inode as a
    # link to the first, so the download carries the window once. Each bundle
    # still works when extracted or copied on its own, which a symlink into the
    # other bundle would not survive. rtk rides along because it is resolved as
    # a sibling of the running executable.
    ln "$MACOS/opencrabs-gui" "$DESKTOP_MACOS/opencrabs-gui"
    ln "$MACOS/rtk" "$DESKTOP_MACOS/rtk"
    install -m 0644 "$ICNS" "$DESKTOP_RESOURCES/icon.icns"

    cat > "$DESKTOP_CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>OpenCrabs Desktop</string>
    <key>CFBundleDisplayName</key>
    <string>OpenCrabs Desktop</string>
    <key>CFBundleIdentifier</key>
    <string>com.opencrabs.desktop</string>
    <key>CFBundleExecutable</key>
    <string>opencrabs-gui</string>
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

    plutil -lint "$DESKTOP_CONTENTS/Info.plist" > /dev/null

    echo "built $DESKTOP_APP (version $VERSION)"
fi
