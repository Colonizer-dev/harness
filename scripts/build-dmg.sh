#!/bin/sh
# Builds Colonizer-arm64.dmg: a Colonizer.app around a release's darwin-arm64 tarball.
#
#   scripts/build-dmg.sh <colonizer-darwin-arm64.tar.gz> <version> <out.dmg>
#
# <version> is the release tag (v0.2.8) or a dev-<sha> name. The release workflow's macOS bundle job
# calls this after it has built the tarball. The app carries that same tarball (the same binary and
# vendored payload) and scripts/install-release.sh; scripts/dmg/launcher.sh is what runs when it is
# opened (see there). The DMG also holds an /Applications link, and — unsigned builds only — a
# "First open.txt" about the one-time "Open Anyway" approval.
#
# COLONIZER_DMG_IDENTITY names a codesigning identity, or "-" (the default) for the ad hoc
# signature Apple silicon requires of any binary. A real identity is a Developer ID Application
# certificate, which the release workflow imports on a tag push when the APPLE_* secrets are set
# (issue #1138): the app is then signed inside out with --options runtime --timestamp, the DMG is
# signed too, and the caller notarizes and staples what this builds (docs/install.md#macos-dmg).
set -eu

[ "$#" -eq 3 ] || { echo "usage: $0 <colonizer-darwin-arm64.tar.gz> <version> <out.dmg>" >&2; exit 2; }
tarball=$1 version=$2 out=$3
repo=$(cd "$(dirname "$0")/.." && pwd)
identity=${COLONIZER_DMG_IDENTITY:--}
[ -f "$tarball" ] || { echo "no such tarball: $tarball" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || { echo "build-dmg.sh needs macOS (hdiutil, codesign)" >&2; exit 1; }

# CFBundleVersion must be dotted numbers: v0.2.8 -> 0.2.8, and a dev build is 0.0.0.
short=${version#v}
case "$short" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) short=0.0.0 ;;
esac
# What the binary needs, read from it (LC_BUILD_VERSION minos), so the plist cannot drift from it.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xzf "$tarball" -C "$work" colonizer/bin/colonizer colonizer/VERSION
minos=$(otool -l "$work/colonizer/bin/colonizer" | awk '/LC_BUILD_VERSION/ { f = 1 } f && $1 == "minos" { print $2; exit }')
[ -n "$minos" ] || minos=11.0

stage=$work/stage
bundle=$stage/Colonizer.app
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$repo/scripts/dmg/launcher.sh" "$bundle/Contents/MacOS/Colonizer"
cp "$repo/scripts/install-release.sh" "$bundle/Contents/Resources/install-release.sh"
cp "$tarball" "$bundle/Contents/Resources/colonizer-darwin-arm64.tar.gz"
cp "$work/colonizer/VERSION" "$bundle/Contents/Resources/VERSION"
chmod 755 "$bundle/Contents/MacOS/Colonizer"

# LSUIElement: a launcher with no window of its own, so no Dock icon bouncing while it installs.
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.colonizer.Colonizer</string>
  <key>CFBundleName</key><string>Colonizer</string>
  <key>CFBundleDisplayName</key><string>Colonizer</string>
  <key>CFBundleExecutable</key><string>Colonizer</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$short</string>
  <key>CFBundleVersion</key><string>$short</string>
  <key>LSMinimumSystemVersion</key><string>$minos</string>
  <key>LSUIElement</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST
plutil -lint "$bundle/Contents/Info.plist" >/dev/null

# Ad hoc, the default: Apple silicon will not run an unsigned binary at all. With a Developer ID
# identity the bundle is signed in one inside-out pass — the launcher is its only executable,
# Resources are data — with the hardened runtime and a trusted timestamp, as notarization requires.
# --deep would paper over nested code instead of failing on it.
case $identity in
  -)
    codesign --force --deep -s - "$bundle"
    codesign --verify --deep --strict "$bundle"
    ;;
  *)
    codesign --force --options runtime --timestamp --sign "$identity" "$bundle"
    codesign --verify --strict "$bundle"
    ;;
esac

ln -s /Applications "$stage/Applications"
if [ "$identity" = - ]; then
  # Written for the ad hoc signature only: a notarized app has nothing to approve.
  cat > "$stage/First open.txt" <<'TXT'
Colonizer: first open

1. Drag Colonizer to Applications, then open it.
2. macOS blocks it, because this app is not signed with an Apple Developer ID. Open
   System Settings > Privacy & Security, scroll to the message about "Colonizer" and
   click "Open Anyway". Confirm once; it opens normally after that.
3. Colonizer installs itself, starts at login, and opens the cockpit in your browser.

Until the app is signed, macOS Keychain asks again for the secrets Colonizer saved
after each update. That is expected; choose Always Allow.

Prefer a terminal? This does the same and needs no approval:
  curl -fsSL https://colonizer.dev/install.sh | sh

Guide: https://colonizer.dev/docs/install
TXT
fi

mkdir -p "$(dirname "$out")"
rm -f "$out"
hdiutil create -quiet -volname Colonizer -srcfolder "$stage" -format UDZO -fs HFS+ -ov "$out"
# The image itself is signed, so the download is checked before anything in it is run. Like the app,
# it carries a trusted timestamp, which the notarization ticket is stapled against.
case $identity in
  -) ;;
  *) codesign --force --timestamp --sign "$identity" "$out" ;;
esac
echo "built $out ($(du -h "$out" | cut -f1)), app $short, LSMinimumSystemVersion $minos"
