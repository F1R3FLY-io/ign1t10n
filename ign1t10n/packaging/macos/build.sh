#!/bin/sh
# Assemble, sign, package and notarise ign1t10n (spec §11). Run on an
# Apple-silicon macOS runner after the node, embers and gaze jobs.
#
#   build.sh VERSION NODE_BIN EMBERS_BIN GAZE_PKG
#
# Environment: SIGN_APP ("Developer ID Application: ..."), SIGN_PKG
# ("Developer ID Installer: ..."), NOTARY_PROFILE (notarytool keychain
# profile). Without SIGN_APP the app is ad-hoc signed and not notarised.
set -eu
VERSION="$1"; NODE="$2"; EMBERS="$3"; GAZE_PKG="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"; ROOT="$HERE/../.."
OUT="$ROOT/dist"; WORK="$OUT/work"; rm -rf "$WORK"; mkdir -p "$WORK/root/Applications" "$OUT"
APP="$WORK/root/Applications/ign1t10n.app"; C="$APP/Contents"
mkdir -p "$C/MacOS" "$C/Helpers" "$C/Resources" "$C/Library/LaunchAgents"

# Decision 2: Apple silicon only; no lipo.
cargo build --release --locked --target aarch64-apple-darwin --manifest-path "$ROOT/Cargo.toml"
cp "$ROOT/target/aarch64-apple-darwin/release/ign1t10n" "$C/MacOS/ign1t10n"
cp "$NODE" "$C/Helpers/f1r3node"
cp "$EMBERS" "$C/Helpers/embers"
sed -e "s/@VERSION@/$VERSION/" -e "s/@BUILD@/$(date +%Y%m%d%H%M)/" "$HERE/Info.plist" > "$C/Info.plist"
cp "$HERE/LaunchAgents/io.f1r3fly.ign1t10n.supervisor.plist" "$C/Library/LaunchAgents/"
cp "$HERE/uninstall.sh" "$ROOT/versions.toml" "$ROOT/compat.toml" "$C/Resources/"
[ -f "$HERE/ign1t10n.icns" ] && cp "$HERE/ign1t10n.icns" "$C/Resources/"
# Carry F1R3Gaze.app too, so first launch can install it if it is missing
# (app copied without the package, or F1R3Gaze deleted since).
pkgutil --expand-full "$GAZE_PKG" "$WORK/gaze-expanded"
GAZE_APP="$(find "$WORK/gaze-expanded" -maxdepth 4 -name F1R3Gaze.app -type d | head -1)"
[ -n "$GAZE_APP" ] || { echo "no F1R3Gaze.app in $GAZE_PKG"; exit 1; }
ditto "$GAZE_APP" "$C/Resources/F1R3Gaze.app"

# Gates (spec §11.3): arm64 only, no dynamic OpenSSL, no development keys.
for b in "$C/MacOS/ign1t10n" "$C/Helpers/f1r3node" "$C/Helpers/embers"; do
  lipo -archs "$b" | grep -qx arm64 || { echo "$b is not arm64-only"; exit 1; }
  if otool -L "$b" | grep -Eiq 'libssl|libcrypto|/opt/homebrew|/usr/local'; then echo "$b links a non-system library"; otool -L "$b"; exit 1; fi
done
if grep -rqaE '04fa70d7be5eb750e0915c0f6d19e7085d18bb1c22d030feb2a877ca2cd226d04438aa819359c56c720142fbc66e9da03a5ab960a3d8b75363a226b7c800f60420|5f668a7ee96d944a4494cc947e4005e172d7ab3461ee5538f1f2a45a835e9657' "$APP"; then
  echo "a development key is in the bundle"; exit 1
fi

codesign --verify --deep --strict "$C/Resources/F1R3Gaze.app"
# Sign inside out with the hardened runtime.
SIGN="${SIGN_APP:--}"
for b in "$C/Helpers/f1r3node" "$C/Helpers/embers" "$C/MacOS/ign1t10n"; do
  codesign --force --options runtime --timestamp --entitlements "$HERE/entitlements.plist" -s "$SIGN" "$b"
done
codesign --force --options runtime --timestamp --entitlements "$HERE/entitlements.plist" -s "$SIGN" "$APP"
codesign --verify --deep --strict "$APP"

pkgbuild --root "$WORK/root" --identifier io.f1r3fly.ign1t10n --version "$VERSION" \
  --scripts "$HERE/scripts" --install-location / "$WORK/ign1t10n-component.pkg"
cp "$GAZE_PKG" "$WORK/f1r3gaze.pkg"
GAZE_VERSION="$(pkgutil --expand "$GAZE_PKG" "$WORK/gx" >/dev/null && sed -n 's/.*version="\([^"]*\)".*/\1/p' "$WORK/gx/PackageInfo" | head -1)"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@GAZE_VERSION@/$GAZE_VERSION/g" "$HERE/Distribution.xml" > "$WORK/Distribution.xml"
mkdir -p "$WORK/resources"; cp "$ROOT/LICENSE" "$WORK/resources/LICENSE.txt"
cp "$HERE/welcome.html" "$HERE/conclusion.html" "$WORK/resources/"
PKG="$OUT/ign1t10n-$VERSION-macos-arm64.pkg"
if [ -n "${SIGN_PKG:-}" ]; then
  productbuild --distribution "$WORK/Distribution.xml" --package-path "$WORK" --resources "$WORK/resources" --sign "$SIGN_PKG" "$PKG"
else
  productbuild --distribution "$WORK/Distribution.xml" --package-path "$WORK" --resources "$WORK/resources" "$PKG"
fi
if [ -n "${NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$PKG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$PKG"
fi
shasum -a 256 "$PKG" > "$PKG.sha256"
echo "$PKG"
