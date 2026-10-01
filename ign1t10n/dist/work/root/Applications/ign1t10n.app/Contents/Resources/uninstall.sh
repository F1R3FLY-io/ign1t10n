#!/bin/sh
# Uninstall ign1t10n (spec §12.3). Shipped in Contents/Resources. Removes the
# local shard, its logs, keys and Keychain items and F1R3Gaze's managed
# settings; leaves F1R3Gaze, its profile and wallets alone.
#   uninstall.sh [--keep-archive]
set -eu
APP="/Applications/ign1t10n.app"
BIN="$APP/Contents/MacOS/ign1t10n"
if [ -x "$BIN" ]; then
  "$BIN" ctl uninstall --yes "$@" || echo "warning: ign1t10n ctl uninstall reported an error; continuing"
fi
/usr/bin/pkill -x ign1t10n 2>/dev/null || true
echo "Removing $APP (administrator password may be requested)"
/usr/bin/sudo /bin/rm -rf "$APP"
/usr/bin/sudo /usr/sbin/pkgutil --forget io.f1r3fly.ign1t10n >/dev/null 2>&1 || true
echo "ign1t10n is uninstalled. F1R3Gaze and its wallets were not touched."
