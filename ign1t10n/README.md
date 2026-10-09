# ign1t10n

The macOS installer for a local F1R3FLY shard. One package installs
**F1R3Gaze** and **ign1t10n**; at first launch ign1t10n asks how many
validators to run (2–10) and whether to bundle **Embers** and
**F1R3Games**, then generates fresh keys, performs a genesis that gives your
F1R3Gaze wallet 10,000,000 F1R3, starts a bootstrap node, the validators, an
observer and Embers on 127.0.0.1, and points F1R3Gaze at them. With
F1R3Games, it installs the portal and game environments on the shard,
registers F1R3Pix, F1R3Beat and F1R3Ink, runs F1R3Ink's relay (anonymous
ink), and serves the portal at `http://localhost:40700` (each game on an
origin of its own), opening it in your web browser: sign on, launch a game,
browse its gallery. A menu-bar item shows the shard's and F1R3Games' health,
starts a new game or opens a gallery in your browser, and lets you resize
the shard, fund wallets, reset or uninstall.

Apple silicon, macOS 13 or later.

* Specification: `docs/ign1t10n-macos-installer-spec` (v0.5 governs this code)
* Implementation notes, decisions and verification status: `docs/IMPLEMENTATION.md`
* Node changes this depends on: `docs/node-work-packages.md`

Build and test (any Unix):

```
cargo test -- --test-threads=1
```

Package (Apple-silicon Mac): `packaging/macos/make-games.sh F1R3GAMES_CHECKOUT games-out`,
then `packaging/macos/build.sh VERSION NODE EMBERS GAZE_PKG games-out`.
