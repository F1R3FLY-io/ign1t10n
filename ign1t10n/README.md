# ign1t10n

The macOS installer for a local F1R3FLY shard. One package installs
**F1R3Gaze** and **ign1t10n**; at first launch ign1t10n asks how many
validators to run (2–10) and whether to bundle **Embers**, then generates
fresh keys, performs a genesis that gives your F1R3Gaze wallet
10,000,000 F1R3, starts a bootstrap node, the validators, an observer and
Embers on 127.0.0.1, and points F1R3Gaze at them. A menu-bar item shows the
shard's health and lets you resize it, fund wallets, reset or uninstall.

Apple silicon, macOS 13 or later.

* Specification: `docs/ign1t10n-macos-installer-spec` (v0.3 governs this code)
* Implementation notes, decisions and verification status: `docs/IMPLEMENTATION.md`
* Node changes this depends on: `docs/node-work-packages.md`

Build and test (any Unix):

```
cargo test -- --test-threads=1
```

Package (Apple-silicon Mac): `packaging/macos/build.sh VERSION NODE EMBERS GAZE_PKG`.
