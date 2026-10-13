#!/usr/bin/env bash
# macOS packaging: build the release binary for the host arch and produce
#   target/package/arbos-<version>-macos-<arch>.dmg          (user download)
#   target/package/arbos-<version>-macos-<arch>-app.tar.gz   (auto-updater)
# containing Arbos.app (unsigned unless CODESIGN_IDENTITY is set). The
# "Release main" workflow runs this on every green commit to main, signed with
# the best Apple certificate the repository holds, and publishes the tarball
# to the `edge` release with Arbos's own Ed25519 signature over its bytes,
# which is what the running app verifies before installing it.
#
# Usage: scripts/package-macos.sh
# Env:   ARBOS_BUILD=<n> appends the build number as a fourth version component
#        (the binary, the bundle, and the file names all carry it); CI passes
#        the commit count, making the version 0.2.112.<n> — the full build
#        version the updater orders on.
#        ARBOS_FX_BINARY=<path> bundles that fx instead of building ../fx with
#        zig. Without it zig must be on PATH: the bundle always carries fx.
#        CODESIGN_IDENTITY="Developer ID Application: …" to sign the bundle.
#        NOTARY_KEY_PATH + NOTARY_KEY_ID + NOTARY_ISSUER_ID — App Store Connect
#          API key (.p8) for notarization; all three set → notarize + staple the
#          app and the dmg, which removes the Gatekeeper warning entirely.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
BASE="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
if [[ -n "${ARBOS_BUILD:-}" ]]; then
  if [[ ! "$ARBOS_BUILD" =~ ^[0-9]+$ ]]; then
    echo "ARBOS_BUILD must be a decimal build number" >&2
    exit 1
  fi
  VERSION="${BASE}.${ARBOS_BUILD}"
  export ARBOS_BUILD
else
  VERSION="$BASE"
fi
ARCH="$(uname -m)" # arm64 on Apple silicon runners
OUT_DIR="$ROOT/target/package"
APP="$OUT_DIR/Arbos.app"
DMG="$OUT_DIR/arbos-$VERSION-macos-$ARCH.dmg"
APP_TARBALL="$OUT_DIR/arbos-$VERSION-macos-$ARCH-app.tar.gz"

cd "$ROOT"
cargo build --release -p zeron

# The agent ships inside the app. The harness runs Contents/MacOS/fx before
# any fx on PATH, so an installed Arbos always runs the fx it was released
# with, and an update to the app is an update to the agent too.
if [[ -n "${ARBOS_FX_BINARY:-}" ]]; then
  FX_BIN="$ARBOS_FX_BINARY"
else
  if ! command -v zig >/dev/null 2>&1; then
    echo "zig is not on PATH: it builds ../fx for the bundle (or set ARBOS_FX_BINARY to a prebuilt fx)" >&2
    exit 1
  fi
  (cd "$ROOT/../fx" && zig build -Doptimize=ReleaseSafe)
  FX_BIN="$ROOT/../fx/zig-out/bin/fx"
fi
if [[ ! -x "$FX_BIN" ]]; then
  echo "fx binary not found at $FX_BIN" >&2
  exit 1
fi

rm -rf "$APP" "$DMG" "$APP_TARBALL"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# The cargo binary is `zeron`; the bundle presents as Arbos, and the updater
# stages exactly this name (Arbos.app/Contents/MacOS/Arbos).
install -m 755 "$ROOT/target/release/zeron" "$APP/Contents/MacOS/Arbos"
install -m 755 "$FX_BIN" "$APP/Contents/MacOS/fx"
sed "s/__VERSION__/$VERSION/" "$ROOT/dist/macos/Info.plist" >"$APP/Contents/Info.plist"
mkdir -p "$APP/Contents/Resources/licenses/fonts"
cp "$ROOT/crates/ui/assets/fonts/licenses/"* "$APP/Contents/Resources/licenses/fonts/"
cp "$ROOT/THIRD_PARTY_NOTICES.md" "$ROOT/LICENSE" "$APP/Contents/Resources/licenses/"

mkdir -p "$APP/Contents/Resources/licenses"
cp "$ROOT/crates/voice/NOTICE.md" "$APP/Contents/Resources/licenses"/parakeet-v3.txt

# Icon: iconset from the pre-masked macOS icon (squircle + margins + shadow
# baked into dist/macos/icon-1024.png — sips can't alpha-mask, so the mask is
# applied ahead of time; dist/zeron.png stays the full-bleed shared artwork).
ICONSET="$OUT_DIR/arbos.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ROOT/dist/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  retina=$((size * 2))
  sips -z "$retina" "$retina" "$ROOT/dist/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/arbos.icns"
rm -rf "$ICONSET"

if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  # Hardened runtime + secure timestamp are both notarization requirements.
  # No --deep (Apple deprecated it): the nested fx executable is signed
  # first, on its own, then the bundle.
  codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP/Contents/MacOS/fx"
  codesign --entitlements "$ROOT/dist/macos/Dictation.entitlements" --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
else
  # Ad-hoc signature so the app launches on Apple silicon (Gatekeeper still
  # requires right-click → Open on first launch without notarization).
  codesign --force --sign - "$APP/Contents/MacOS/fx"
  codesign --deep --force --entitlements "$ROOT/dist/macos/voice.entitlements" --sign - "$APP"
fi

# notarize <path>: submit to Apple and wait for the verdict. A rejection may
# still exit 0 depending on the notarytool version — the `stapler staple` that
# follows each call has no ticket to attach then, and fails the build for us.
notarize() {
  xcrun notarytool submit "$1" \
    --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" \
    --issuer "$NOTARY_ISSUER_ID" --wait
}
# staple <path>: fetch the ticket from Apple and attach it. The fetch goes to
# api.apple-cloudkit.com and now and then the connection drops mid-request
# (stapler exit 68, "The network connection was lost"), which is not a verdict
# on the build; a missing ticket after the retries still fails the build.
staple() {
  local attempt
  for attempt in 1 2 3 4; do
    if xcrun stapler staple "$1"; then
      return 0
    fi
    echo "stapler attempt $attempt failed; retrying in $((attempt * 15))s" >&2
    sleep $((attempt * 15))
  done
  xcrun stapler staple "$1"
}
NOTARIZE=false
[[ -n "${NOTARY_KEY_PATH:-}" && -n "${NOTARY_KEY_ID:-}" && -n "${NOTARY_ISSUER_ID:-}" ]] && NOTARIZE=true

if $NOTARIZE; then
  # Staple the bundle BEFORE tarring it: the auto-updater swaps the .app with
  # no dmg involved, so the tarball copy must carry its own ticket to pass
  # Gatekeeper offline.
  ZIP="$OUT_DIR/arbos-notarize.zip"
  ditto -c -k --keepParent "$APP" "$ZIP"
  notarize "$ZIP"
  rm -f "$ZIP"
  staple "$APP"
fi

# The auto-updater artifact.
tar -czf "$APP_TARBALL" -C "$OUT_DIR" Arbos.app
echo "packaged: $APP_TARBALL"

# The dmg presents the classic drag-into-Applications layout over the
# ascii-hands artwork (committed renders from scripts/dmg-background.py).
# dmgbuild writes the .DS_Store (background, icon view, icon positions)
# directly — no Finder scripting, so it also works on headless CI runners.
python3 -c 'import dmgbuild' 2>/dev/null ||
  python3 -m pip install --quiet --user dmgbuild 2>/dev/null ||
  python3 -m pip install --quiet --user --break-system-packages dmgbuild

# Pair the 1x/2x background renders into a hidpi tiff so the artwork stays
# crisp on retina displays.
BG_TIFF="$OUT_DIR/dmg-background.tiff"
tiffutil -cathidpicheck "$ROOT/dist/macos/dmg-background.png" \
  "$ROOT/dist/macos/dmg-background@2x.png" -out "$BG_TIFF" >/dev/null 2>&1

APP="$APP" DMG="$DMG" BG_TIFF="$BG_TIFF" python3 - <<'PY'
import os
import dmgbuild

app = os.environ["APP"]
dmgbuild.build_dmg(
    filename=os.environ["DMG"],
    volume_name="Arbos",
    settings={
        "format": "UDZO",
        "files": [app],
        "symlinks": {"Applications": "/Applications"},
        "icon": os.path.join(app, "Contents/Resources/arbos.icns"),
        "background": os.environ["BG_TIFF"],
        "show_status_bar": False,
        "show_tab_view": False,
        "show_toolbar": False,
        "show_pathbar": False,
        "show_sidebar": False,
        "default_view": "icon-view",
        # Window and icon geometry must match scripts/dmg-background.py.
        "window_rect": ((200, 120), (660, 400)),
        "icon_size": 104,
        "text_size": 12,
        "icon_locations": {"Arbos.app": (165, 195), "Applications": (495, 195)},
    },
)
PY
rm -f "$BG_TIFF"
if $NOTARIZE; then
  notarize "$DMG"
  staple "$DMG"
fi
echo "packaged: $DMG"
