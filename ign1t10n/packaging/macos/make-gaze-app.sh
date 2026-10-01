#!/bin/sh
# Build an Apple-silicon-only F1R3Gaze.app from a F1R3Gaze checkout, laid out
# exactly as F1R3Gaze's own packaging/macos/package.sh lays out its bundle,
# but without the x86_64 half (ign1t10n is Apple silicon only, Decision 2).
#
#   make-gaze-app.sh F1R3GAZE_CHECKOUT VERSION OUT_DIR
#
# F1R3GAZE_CHECKOUT is the F1R3Gaze repository root or the Cargo workspace
# inside it (…/F1R3Gaze/f1r3-work/f1r3gaze). Prints the path of the F1R3Gaze.app made.
# Signed with MACOS_SIGN_IDENTITY when set, else ad-hoc (local testing).
set -eu
ARG="${1:?F1R3Gaze checkout}"; VER="${2:?version}"; OUT="${3:?output dir}"
[ -d "$ARG" ] || { echo "no such directory: $ARG (where is your F1R3Gaze checkout?)"; exit 1; }
WS="$(cd "$ARG" && pwd)"
# Accept the repository root as well as the workspace inside it.
[ -f "$WS/packaging/macos/Info.plist" ] || [ ! -d "$WS/f1r3-work/f1r3gaze" ] || WS="$WS/f1r3-work/f1r3gaze"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
PK="$WS/packaging/macos"; ICONS="$WS/packaging/icons"
[ -f "$PK/Info.plist" ] || { echo "$WS is not F1R3Gaze's workspace (no packaging/macos/Info.plist)"; exit 1; }

# Build with F1R3Gaze's own toolchain (its rust-toolchain.toml), for arm64.
( cd "$WS" && rustup target add aarch64-apple-darwin >/dev/null && cargo build --release --target aarch64-apple-darwin --bin f1r3gaze --bin f1r3c )
BIN="$WS/target/aarch64-apple-darwin/release"

APP="$OUT/F1R3Gaze.app"; C="$APP/Contents"
rm -rf "$APP"; mkdir -p "$C/MacOS" "$C/Resources"
cp "$BIN/f1r3gaze" "$BIN/f1r3c" "$C/MacOS/"
sed "s/__VERSION__/$VER/g" "$PK/Info.plist" > "$C/Info.plist"

IS="$(mktemp -d)/f1r3gaze.iconset"; mkdir -p "$IS"
for s in 16 32 128 256 512; do
  cp "$ICONS/$s.png" "$IS/icon_${s}x${s}.png"
  cp "$ICONS/$((s * 2)).png" "$IS/icon_${s}x${s}@2x.png" 2>/dev/null || cp "$ICONS/1024.png" "$IS/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$IS" -o "$C/Resources/f1r3gaze.icns"

SIGN="${MACOS_SIGN_IDENTITY:--}"
TS="--timestamp"; [ "$SIGN" = "-" ] && TS="--timestamp=none"
for b in "$C/MacOS/f1r3c" "$C/MacOS/f1r3gaze" "$APP"; do
  codesign --force $TS --options runtime --entitlements "$PK/entitlements.plist" --sign "$SIGN" "$b"
done
codesign --verify --deep --strict "$APP"
echo "$APP"
