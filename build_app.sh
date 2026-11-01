#!/usr/bin/env bash
#
# build_app.sh — builds MD-Viewer and packages it as a native macOS .app bundle.
#
# Usage:
#   ./build_app.sh                 # build MD-Viewer.app (ad-hoc signed)
#   ./build_app.sh --package       # build + dist/MD-Viewer-<version>-arm64.zip
#   ./build_app.sh --install       # build + replace /Applications/MD-Viewer.app
#
# Distribution signing / notarization (optional, requires an Apple Developer
# account and a "Developer ID Application" certificate in the keychain):
#
#   CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
#   NOTARY_PROFILE="mdviewer-notary" \
#   ./build_app.sh --package
#
#   Create the notary profile once with:
#     xcrun notarytool store-credentials mdviewer-notary \
#       --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
#
# Without CODESIGN_IDENTITY the bundle is ad-hoc signed (fine for local use).
# Without NOTARY_PROFILE the archive is not notarized; macOS then requires
# right-click → Open on first launch (see README).
#
# Apple Silicon only: the release binary is built for arm64. Build on an
# Apple Silicon Mac; the script refuses to package anything else.

set -euo pipefail

APP_NAME="MD-Viewer"
BIN_NAME="md_viewer"
BUNDLE_ID="${BUNDLE_ID:-com.kauschke.mdviewer}"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
# Build number shown as "Version <short> (<build>)" in the About panel.
BUILD_NUMBER="${BUILD_NUMBER:-1}"

INSTALL=0
PACKAGE=0
for arg in "$@"; do
  case "$arg" in
    --install) INSTALL=1 ;;
    --package) PACKAGE=1 ;;
    --help|-h)
      awk 'NR > 2 && !/^#/ { exit } NR > 2 { sub(/^# ?/, ""); print }' "$0"
      exit 0
      ;;
    *)
      echo "error: unknown argument '$arg' (use --install, --package or --help)" >&2
      exit 1
      ;;
  esac
done

cd "$(dirname "$0")"

# Single source of truth for the version.
VERSION="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)"
if [ -z "$VERSION" ]; then
  echo "error: cannot read the package version from Cargo.toml" >&2
  exit 1
fi

if [ "$(uname -m)" != "arm64" ]; then
  echo "error: MD-Viewer is published for Apple Silicon (arm64) only." >&2
  echo "       Build on an Apple Silicon Mac." >&2
  exit 1
fi

echo "==> Building release binary (MD-Viewer $VERSION, arm64)..."
cargo build --release

BINARY="target/release/$BIN_NAME"
if [ ! -f "$BINARY" ]; then
  echo "error: expected binary not found: $BINARY" >&2
  exit 1
fi
if ! lipo -archs "$BINARY" | grep -qw arm64; then
  echo "error: $BINARY is not an arm64 binary (found: $(lipo -archs "$BINARY"))" >&2
  exit 1
fi

APP_DIR="$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RESOURCES_DIR="$CONTENTS/Resources"

echo "==> Assembling ${APP_DIR}..."
rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

cp "$BINARY" "$MACOS_DIR/$APP_NAME"
chmod +x "$MACOS_DIR/$APP_NAME"

# Optional app icon.
ICON_ENTRY=""
if [ -f "assets/AppIcon.icns" ]; then
  cp "assets/AppIcon.icns" "$RESOURCES_DIR/AppIcon.icns"
  ICON_ENTRY='<key>CFBundleIconFile</key><string>AppIcon</string>'
fi

# Startup document. Optional: if it is missing, the app opens an empty
# document instead of failing.
if [ -f "start-up.md" ]; then
  cp "start-up.md" "$RESOURCES_DIR/start-up.md"
fi


cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$BUILD_NUMBER</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>$APP_NAME</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 JoKa. MIT License.</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSArchitecturePriority</key>
  <array><string>arm64</string></array>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  $ICON_ENTRY
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Markdown Document</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>net.daringfireball.markdown</string>
      </array>
    </dict>
  </array>
  <key>UTImportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>net.daringfireball.markdown</string>
      <key>UTTypeDescription</key><string>Markdown Document</string>
      <key>UTTypeConformsTo</key>
      <array><string>public.plain-text</string></array>
      <key>UTTypeTagSpecification</key>
      <dict>
        <key>public.filename-extension</key>
        <array><string>md</string><string>markdown</string><string>mdown</string></array>
      </dict>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Validate the generated plist (fails fast on malformed XML).
plutil -lint "$CONTENTS/Info.plist" >/dev/null

echo "==> Signing ($CODESIGN_IDENTITY)…"
if [ "$CODESIGN_IDENTITY" = "-" ]; then
  codesign --force --sign - "$APP_DIR" >/dev/null
else
  codesign --force --options runtime --timestamp \
    --sign "$CODESIGN_IDENTITY" "$APP_DIR" >/dev/null
fi
codesign --verify --deep --strict "$APP_DIR"

# Register the bundle with LaunchServices so Finder picks up the document
# types immediately (also refreshes a stale association after a rebuild).
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

package_app() {
  mkdir -p dist
  local name="${APP_NAME}-${VERSION}-arm64.zip"
  local zip="dist/$name"
  rm -f "$zip" "$zip.sha256"

  if [ -n "${NOTARY_PROFILE:-}" ]; then
    if [ "$CODESIGN_IDENTITY" = "-" ]; then
      echo "error: NOTARY_PROFILE is set but the bundle is ad-hoc signed." >&2
      echo "       Set CODESIGN_IDENTITY to a Developer ID Application certificate." >&2
      exit 1
    fi
    # notarytool needs an archive to submit; it is discarded, because the
    # stapled bundle must be archived again for the ticket to ship.
    local submit_dir
    submit_dir="$(mktemp -d)"
    echo "==> Notarizing (profile: $NOTARY_PROFILE)…"
    ditto -c -k --keepParent --norsrc --noextattr "$APP_DIR" "$submit_dir/$name"
    xcrun notarytool submit "$submit_dir/$name" --keychain-profile "$NOTARY_PROFILE" --wait
    rm -rf "$submit_dir"
    echo "==> Stapling notarization ticket…"
    xcrun stapler staple "$APP_DIR"
    xcrun stapler validate "$APP_DIR"
  fi

  echo "==> Creating release archive…"
  ditto -c -k --keepParent --norsrc --noextattr "$APP_DIR" "$zip"
  # Checksum from inside dist/, so `shasum -c` works on the file as shipped.
  (cd dist && shasum -a 256 "$name" > "$name.sha256")

  echo
  echo "Package: $zip"
  echo "SHA-256: $(cut -d' ' -f1 "$zip.sha256")"
}

if [ "$PACKAGE" = "1" ]; then
  package_app
fi

if [ "$INSTALL" = "1" ]; then
  echo "==> Installing to /Applications…"
  rm -rf "/Applications/$APP_NAME.app"
  ditto "$APP_DIR" "/Applications/$APP_NAME.app"
  if [ -x "$LSREG" ]; then
    "$LSREG" -f "/Applications/$APP_NAME.app" >/dev/null 2>&1 || true
    # Drop the local build copy from LaunchServices before deleting it.
    "$LSREG" -u "$PWD/$APP_DIR" >/dev/null 2>&1 || true
  fi
  rm -rf "$APP_DIR"
  echo
  echo "Installed: /Applications/$APP_NAME.app"
  echo "Open with:"
  echo "  open \"/Applications/$APP_NAME.app\""
else
  if [ -x "$LSREG" ]; then
    "$LSREG" -f "$PWD/$APP_DIR" >/dev/null 2>&1 || true
  fi

  echo
  echo "Done: $APP_DIR"
  echo "Create a distributable archive with:"
  echo "  ./build_app.sh --package"
  echo "Install locally with (replaces any installed copy, avoids a duplicate icon):"
  echo "  ./build_app.sh --install"
fi
