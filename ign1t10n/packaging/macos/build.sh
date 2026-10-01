#!/bin/sh
# Assemble, sign, package and notarise ign1t10n (spec §11). Run on an
# Apple-silicon macOS runner after the node, embers and gaze jobs.
#
#   build.sh VERSION NODE_BIN EMBERS_BIN GAZE
#
# GAZE is F1R3Gaze as its packaging produces it (F1R3Gaze-<v>-macos-*.dmg),
# a .pkg containing F1R3Gaze.app, or a F1R3Gaze.app directory.
#
# Environment: SIGN_APP ("Developer ID Application: ..."), SIGN_PKG
# ("Developer ID Installer: ..."), NOTARY_PROFILE (notarytool keychain
# profile). Without SIGN_APP everything is ad-hoc signed and not notarised:
# fine on the Mac that built it, blocked by Gatekeeper anywhere else.
# ALLOW_NONSYSTEM_LIBS=1 lets a node linked to Homebrew's OpenSSL through
# (local testing only, until work package N2): the package then works only
# on Macs with that Homebrew library.
set -eu
VERSION="$1"; NODE="$2"; EMBERS="$3"; GAZE_IN="$4"
[ $# -eq 4 ] || { echo "usage: build.sh VERSION NODE_BIN EMBERS_BIN GAZE(.dmg|.pkg|.app)"; exit 1; }
[ -x "$NODE" ] || { echo "node binary not found or not executable: $NODE"; exit 1; }
[ -x "$EMBERS" ] || { echo "embers binary not found or not executable: $EMBERS"; exit 1; }
[ -e "$GAZE_IN" ] || { echo "F1R3Gaze not found: $GAZE_IN (make it with packaging/macos/make-gaze-app.sh, or pass /Applications/F1R3Gaze.app)"; exit 1; }
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
# F1R3Gaze: get F1R3Gaze.app out of whatever form it came in.
GAZE_APP="$WORK/gaze/F1R3Gaze.app"; mkdir -p "$WORK/gaze"
case "$GAZE_IN" in
  *.dmg)
    MNT="$WORK/gaze-mnt"; mkdir -p "$MNT"
    hdiutil attach -nobrowse -readonly -mountpoint "$MNT" "$GAZE_IN" >/dev/null
    ditto "$MNT/F1R3Gaze.app" "$GAZE_APP"
    hdiutil detach "$MNT" >/dev/null ;;
  *.pkg)
    pkgutil --expand-full "$GAZE_IN" "$WORK/gaze-expanded"
    FOUND="$(find "$WORK/gaze-expanded" -maxdepth 5 -name F1R3Gaze.app -type d | head -1)"
    [ -n "$FOUND" ] || { echo "no F1R3Gaze.app in $GAZE_IN"; exit 1; }
    ditto "$FOUND" "$GAZE_APP" ;;
  *.app) ditto "$GAZE_IN" "$GAZE_APP" ;;
  *) echo "GAZE must be a .dmg, .pkg or .app: $GAZE_IN"; exit 1 ;;
esac
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$GAZE_APP/Contents/Info.plist")" = io.f1r3fly.f1r3gaze ] \
  || { echo "$GAZE_IN does not contain F1R3Gaze (io.f1r3fly.f1r3gaze)"; exit 1; }
GAZE_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$GAZE_APP/Contents/Info.plist")"
if ! codesign --verify --deep --strict "$GAZE_APP" 2>/dev/null; then
  if [ -n "${SIGN_APP:-}" ]; then echo "F1R3Gaze is not validly signed; a release needs a signed F1R3Gaze"; exit 1; fi
  echo "warning: F1R3Gaze is unsigned; ad-hoc signing it for a local test" >&2
  codesign --force --deep -s - "$GAZE_APP"
fi
# Carry a copy, so first launch can install it if it is missing.
ditto "$GAZE_APP" "$C/Resources/F1R3Gaze.app"

# Gates (spec §11.3): arm64 only, no dynamic OpenSSL, no development keys.
LOCAL_LIBS=""
for b in "$C/MacOS/ign1t10n" "$C/Helpers/f1r3node" "$C/Helpers/embers"; do
  lipo -archs "$b" | grep -qx arm64 || { echo "$b is not arm64-only"; exit 1; }
  if otool -L "$b" | grep -Eiq 'libssl|libcrypto|/opt/homebrew|/usr/local'; then
    echo "$b links a non-system library:"; otool -L "$b" | grep -Ei 'libssl|libcrypto|/opt/homebrew|/usr/local'
    if [ "${ALLOW_NONSYSTEM_LIBS:-}" = 1 ] && [ -z "${SIGN_APP:-}" ]; then
      echo "warning: allowed for a local test build; $(basename "$b") is signed to permit loading them" >&2
      LOCAL_LIBS="$LOCAL_LIBS $b"
    else exit 1; fi
  fi
done
# No development *private* key (node repo docker/.env.example) may ship.
# (Development public keys are expected: ign1t10n carries them to refuse them.)
DEV_PRIVATE='5f668a7ee96d944a4494cc947e4005e172d7ab3461ee5538f1f2a45a835e9657|357cdc4201a5650830e0bc5a03299a30038d9934ba4c7ab73ec164ad82471ff9|2c02138097d019d263c1d5383fcaddb1ba6416a0f4e64e3a617fe3af45b7851d|b67533f1f99c0ecaedb7d829e430b1c0e605bda10f339f65d5567cb5bd77cbcb|5ff3514bf79a7d18e8dd974c699678ba63b7762ce8d78c532346e52f0ad219cd'
LEAKS="$(grep -rlaE "$DEV_PRIVATE" "$APP" || true)"
if [ -n "$LEAKS" ]; then
  echo "a development private key is in the bundle, in:"; echo "$LEAKS"
  if [ "${ALLOW_DEV_KEYS:-}" = 1 ] && [ -z "${SIGN_APP:-}" ]; then echo "warning: allowed for a local test build" >&2; else exit 1; fi
fi

# Every component (the app, and the F1R3Gaze it carries) installs exactly
# where it is in the package: Installer must never "relocate" one onto some
# other copy it finds on the Mac.
no_relocation() {
  i=0
  while /usr/libexec/PlistBuddy -c "Print :$i" "$1" >/dev/null 2>&1; do
    /usr/libexec/PlistBuddy -c "Set :$i:BundleIsRelocatable false" "$1"
    i=$((i + 1))
  done
}

# Sign inside out with the hardened runtime.
SIGN="${SIGN_APP:--}"
# Secure timestamps need a real identity; ad-hoc signatures cannot have one.
TS="--timestamp"; [ "$SIGN" = "-" ] && TS="--timestamp=none"
for b in "$C/Helpers/f1r3node" "$C/Helpers/embers" "$C/MacOS/ign1t10n"; do
  ENT="$HERE/entitlements.plist"
  case " $LOCAL_LIBS " in *" $b "*) ENT="$HERE/entitlements-local-libs.plist" ;; esac
  codesign --force --options runtime $TS --entitlements "$ENT" -s "$SIGN" "$b"
done
if [ "$SIGN" = "-" ]; then
  # Ad-hoc: give the app a designated requirement that does not change from
  # build to build (an ad-hoc default names the exact code hash), so macOS
  # can recognise a rebuilt app as the one whose background item it approved.
  codesign --force --options runtime $TS --entitlements "$HERE/entitlements.plist" -s - \
    -r='designated => identifier "io.f1r3fly.ign1t10n"' "$APP"
else
  codesign --force --options runtime $TS --entitlements "$HERE/entitlements.plist" -s "$SIGN" "$APP"
fi
codesign --verify --deep --strict "$APP"

pkgbuild --analyze --root "$WORK/root" "$WORK/ign1t10n-components.plist"
no_relocation "$WORK/ign1t10n-components.plist"
pkgbuild --root "$WORK/root" --component-plist "$WORK/ign1t10n-components.plist" --identifier io.f1r3fly.ign1t10n \
  --version "$VERSION" --scripts "$HERE/scripts" --install-location / "$WORK/ign1t10n-component.pkg"
# F1R3Gaze's component package, installed to /Applications (never
# "relocated" onto some other copy of F1R3Gaze, e.g. a development build).
mkdir -p "$WORK/gaze-root"; ditto "$GAZE_APP" "$WORK/gaze-root/F1R3Gaze.app"
pkgbuild --analyze --root "$WORK/gaze-root" "$WORK/gaze-components.plist"
no_relocation "$WORK/gaze-components.plist"
pkgbuild --root "$WORK/gaze-root" --component-plist "$WORK/gaze-components.plist" --identifier io.f1r3fly.f1r3gaze \
  --version "$GAZE_VERSION" --install-location /Applications "$WORK/f1r3gaze.pkg"
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
