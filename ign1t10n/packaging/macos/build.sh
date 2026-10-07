#!/bin/sh
# Assemble, sign, package and notarise ign1t10n (spec §11). Run on an
# Apple-silicon macOS runner after the node, embers and gaze jobs.
#
#   build.sh VERSION NODE_BIN EMBERS_BIN GAZE [GAMES]
#
# GAMES is the directory make-games.sh produces (bin/f1r3games-service,
# bin/f1r3games, res/): F1R3Games, spec v0.4. Without it the package carries
# no F1R3Games and first run offers none.
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
VERSION="$1"; NODE="$2"; EMBERS="$3"; GAZE_IN="$4"; GAMES="${5:-}"
[ $# -eq 4 ] || [ $# -eq 5 ] || { echo "usage: build.sh VERSION NODE_BIN EMBERS_BIN GAZE(.dmg|.pkg|.app) [GAMES_DIR]"; exit 1; }
if [ -n "$GAMES" ]; then
  for f in bin/f1r3games-service bin/f1r3games res/games.toml res/portal/index.html; do
    [ -e "$GAMES/$f" ] || { echo "$GAMES is not a make-games.sh output (no $f)"; exit 1; }
  done
fi
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
HELPERS="$C/Helpers/f1r3node $C/Helpers/embers"
if [ -n "$GAMES" ]; then
  cp "$GAMES/bin/f1r3games-service" "$GAMES/bin/f1r3games" "$C/Helpers/"
  ditto "$GAMES/res" "$C/Resources/f1r3games"
  HELPERS="$HELPERS $C/Helpers/f1r3games-service $C/Helpers/f1r3games"
fi
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
for b in "$C/MacOS/ign1t10n" $HELPERS; do
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
# Nor any key committed in F1R3Games' examples/local-shard (upper-case hex there).
DEV_PRIVATE="$DEV_PRIVATE|2168212FF2D7B9B998D57D4B2D355D0D4628172F3C339E54FEC7A4935954F43D|3E25D094FF00B9743975EE6A8F491AECFDA4197D6B678D5ED4D4802D73311729|4471A6DCBC0AE432F5E230E1EBCDC805B0A5C1A770066AD95C2D7F14A9FBF5EA|48AA68D103B45987CBE088313DE77599DFA865A07CF750076D56E6FB0652C8D7|4E0CAA276C16EE2D5C80FFAFD2DBEDA08151CB876AFD807463BD9BFD36EEC3E6|4FACDD3593B8729D60BA3DF2F4926028BE10E8F9BC1004CE35AE9A5CA21D0B16|62FEB32CC6096009CCC00F2ABF743E48159F09EBE7947FF930993F952CE163E7|69097666896F7AFB95A5F9DE95823ED12482E3A72F14213BA2CA22B2BCBF6848|90F0CD0F2A0096997BD5906DE31D37E665624B0B8A2D543E44DF9837D1141B05|97D876B2EFDD32959F666102D96517CC2EF949D47122A4623BCE56A90C0F6D58|BA11C03198F3DF96316AFE0A656FC8ADEF8757F179003033A0215B8BF0D8A3FB|CD0FF183F942C93AFBCF2957FDD3995F4AB680A5B20B4223DF7E14AB971609A7|DE9FCE2B4F6395F68F37F91321C4B54DE7E3672A98FEC2417973DB1F9BB6AF45|E922E7E3DF03BA42008218816090A1227B526C2FCBD0E86663A791EFB8A033E2|EC064A97D17FCA688391F630A0B5760F3F1CF418C288F2070C0E17572B9ED120|15cd4dda2d92c158fde334cb6e86fd837e3d2644e747e5c4d84ae364f0e7905d"
LEAKS="$(grep -rlaiE "$DEV_PRIVATE" "$APP" || true)"
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
for b in $HELPERS "$C/MacOS/ign1t10n"; do
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
