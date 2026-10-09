#!/bin/sh
# Build what ign1t10n bundles of F1R3Games (spec v0.4 §15, job `games`),
# from a checkout of F1R3FLY-io/F1R3Games at the revision versions.toml pins.
#
#   make-games.sh F1R3GAMES_CHECKOUT OUT
#
# Produces OUT/bin/f1r3games-service, OUT/bin/f1r3games (the CLI, Decision
# 16), and OUT/res, which build.sh copies to Contents/Resources/f1r3games:
#   res/games.toml            the revision and the environment versions to install
#   res/portal/               Portal/f1r3games-portal/web/dist (the shell and its wallet .wasm)
#   res/games/<id>/           each listed client's dist (index.html, preview/<kind>.html)
#
# Needs Rust >= 1.85 with the aarch64-apple-darwin target (on macOS) and
# Node 22. REBUILD_WASM=1 rebuilds the wallet's WebAssembly (needs the
# wasm32-unknown-unknown target and wasm-bindgen-cli 0.2.129); otherwise the
# module committed in web/src/wasm is used. TARGET overrides the Rust target
# (Linux CI builds the host target to run the end-to-end test).
set -eu
SRC="$1"; OUT="$2"
[ $# -eq 2 ] || { echo "usage: make-games.sh F1R3GAMES_CHECKOUT OUT"; exit 1; }
HERE="$(cd "$(dirname "$0")" && pwd)"; ROOT="$HERE/../.."
PORTAL="$SRC/Portal/f1r3games-portal"
[ -f "$PORTAL/Cargo.toml" ] || { echo "$SRC is not a F1R3Games checkout (no Portal/f1r3games-portal)"; exit 1; }
rm -rf "$OUT"; mkdir -p "$OUT/bin" "$OUT/res/games"

# What to bundle, from versions.toml: the clients and the environment versions.
eval "$(python3 - "$ROOT/versions.toml" <<'PY'
import sys, tomllib
g = tomllib.load(open(sys.argv[1], "rb"))["f1r3games"]
print(f"REVISION={g['revision'][:7]}")
print(f"CLIENTS='{' '.join(g['clients'])}'")
PY
)"

# The service and the CLI.
TARGET="${TARGET:-aarch64-apple-darwin}"
cargo build --release --locked --manifest-path "$PORTAL/Cargo.toml" --target "$TARGET" -p f1r3games-service -p f1r3games-cli
cp "$PORTAL/target/$TARGET/release/f1r3games-service" "$PORTAL/target/$TARGET/release/f1r3games" "$OUT/bin/"

# The shell. Its tests run the real wallet against the real service.
( cd "$PORTAL/web"
  npm ci --include=dev
  [ "${REBUILD_WASM:-}" = 1 ] && npm run wasm
  npm run build )
cp -R "$PORTAL/web/dist" "$OUT/res/portal"

# Each client, at games/<id>/ (served at /<id>/ on the game's own origin).
for id in $CLIENTS; do
  case "$id" in
    f1r3pix) dir="$SRC/F1R3Pix/client" ;;
    f1r3beat) dir="$SRC/F1R3Beat/client" ;;
    f1r3ink) dir="$SRC/F1R3Ink/client" ;;
    *) echo "no client directory known for $id"; exit 1 ;;
  esac
  ( cd "$dir" && npm ci --include=dev && npm test && npm run build )
  cp -R "$dir/dist" "$OUT/res/games/$id"
  [ -f "$OUT/res/games/$id/index.html" ] || { echo "$id: no index.html in its build"; exit 1; }
  # Every gallery kind the game declares needs its renderer (spec v0.5 §10.5).
  for kind in $(python3 - "$PORTAL/crates/games/src/lib.rs" "$id" <<'PY'
import re, sys
src = open(sys.argv[1]).read()
m = re.search(r'id: "%s",.*?galleries: &\[(.*?)\],' % re.escape(sys.argv[2]), src, re.S)
print(" ".join(re.findall(r'kind: "([a-z0-9_-]+)"', m.group(1))) if m else "")
PY
  ); do
    [ -f "$OUT/res/games/$id/preview/$kind.html" ] || { echo "$id: no gallery renderer preview/$kind.html in its build"; exit 1; }
  done
done

# games.toml: what ign1t10n installs and registers.
python3 - "$ROOT/versions.toml" "$OUT/res/games.toml" "$CLIENTS" <<'PY'
import sys, tomllib
g = tomllib.load(open(sys.argv[1], "rb"))["f1r3games"]
clients = set(sys.argv[3].split())
ids = ["f1r3pix", "f1r3beat", "f1r3ink", "f1r3sidechat", "f1r3skein"]
v = g.get("game_versions", {})
with open(sys.argv[2], "w") as f:
    f.write(f'# Written by make-games.sh from versions.toml.\nrevision = "{g["revision"][:7]}"\nenv_version = {g.get("env_version", 1)}\n')
    for i in ids:
        f.write(f'\n[[game]]\nid = "{i}"\nenv_version = {v.get(i, 1)}\nclient = {"true" if i in clients else "false"}\n')
PY

# Nothing from the repository's examples may ship (their keys are public);
# build.sh also greps the whole app for their values.
LEAKED="$(find "$OUT/res" \( -name '*key*.json' -o -name 'token-secret*' -o -name 'f1r3games.toml' -o -name 'manifests.json' \) -print)"
[ -z "$LEAKED" ] || { echo "example key or configuration files found their way into the bundle:"; echo "$LEAKED"; exit 1; }
echo "$OUT"
