#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

PROFILE="release"
SKIP_BUILD=false
OFFLINE=false

for arg in "$@"; do
    case "$arg" in
        --debug)
            PROFILE="debug"
            ;;
        --skip-build)
            SKIP_BUILD=true
            ;;
        --offline)
            OFFLINE=true
            ;;
        *)
            echo "Unknown option: $arg" >&2
            echo "Usage: $0 [--debug] [--offline] [--skip-build]" >&2
            exit 1
            ;;
    esac
done

if [[ "$SKIP_BUILD" == false ]]; then
    CARGO_FLAGS=(-p dambo-ui)
    if [[ "$PROFILE" == "release" ]]; then
        CARGO_FLAGS+=(--release)
    fi
    if [[ "$OFFLINE" == true ]]; then
        CARGO_FLAGS+=(--offline)
    fi
    echo "Building dambo-ui ($PROFILE)..."
    cargo build "${CARGO_FLAGS[@]}"
fi

BIN_PATH="$ROOT_DIR/target/$PROFILE/dambo-ui"
if [[ ! -x "$BIN_PATH" ]]; then
    echo "Error: compiled binary not found at $BIN_PATH" >&2
    exit 1
fi

APP_VERSION="$(awk '
    /^\[workspace\.package\]/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^[[:space:]]*version[[:space:]]*=/ {
        sub(/^[^"]*"/, "")
        sub(/".*$/, "")
        gsub(/\r/, "")
        print
        exit
    }
' "$ROOT_DIR/Cargo.toml")"
if [[ -z "$APP_VERSION" ]]; then
    echo "Error: failed to read [workspace.package].version from $ROOT_DIR/Cargo.toml" >&2
    exit 1
fi

APP_DIR="$ROOT_DIR/Dambo.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

cp "$BIN_PATH" "$MACOS_DIR/dambo-ui"
chmod +x "$MACOS_DIR/dambo-ui"

if [[ -f "$ROOT_DIR/src-tauri/icons/icon.icns" ]]; then
    cp "$ROOT_DIR/src-tauri/icons/icon.icns" "$RESOURCES_DIR/icon.icns"
fi

cat > "$CONTENTS_DIR/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>dambo-ui</string>
    <key>CFBundleIdentifier</key>
    <string>com.dambo.app</string>
    <key>CFBundleName</key>
    <string>Dambo</string>
    <key>CFBundleDisplayName</key>
    <string>Dambo</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleIconFile</key>
    <string>icon.icns</string>
    <key>CFBundleShortVersionString</key>
    <string>${APP_VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${APP_VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.15</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSAppTransportSecurity</key>
    <dict>
        <key>NSAllowsLocalNetworking</key>
        <true/>
    </dict>
</dict>
</plist>
EOF

if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$APP_DIR" || echo "Warning: ad-hoc codesign failed; continuing."
fi

echo "Packaged macOS app bundle: $APP_DIR"
