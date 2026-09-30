# ign1t10n: implementation notes

This is the Rust implementation of the macOS installer described in
*ign1t10n: a macOS installer for a local F1R3Node-Rust shard*, **version 0.3**
(bootstrap, 2–10 validators, observer, in-place resizing). The repository's
`docs/` still carries v0.2 (single standalone validator); v0.3 should be
committed beside it.

## Decisions applied

| # | Decision | Where it lives |
|---|----------|----------------|
| 1 | Keep the separate bootstrap for v1 | `provision::allocate_ports`, `procs::node_args` (bootstrap: ceremony master, heartbeat off, no `--bootstrap`; its node id is read from `/api/status`) |
| 2 | Apple silicon only | `versions.toml [platform]`, `Info.plist` (`LSArchitecturePriority`), `Distribution.xml` (`hostArchitectures="arm64"`), `build.sh` (no `lipo`, arm64 gate) |
| 3 | macOS 13.0 minimum | `Info.plist`, `Distribution.xml`, `provision::preflight`, SMAppService in `platform::macos` |
| 4 | 10,000,000 F1R3 to the browser's wallet | `amounts.rs` (`BROWSER_WALLET_F1R3`); validators get 1,000 F1R3 fees and a stake of 1,000; Embers' two service wallets 1,000,000 F1R3 each when bundled; the faucet the rest of a 500,000,000 F1R3 supply (`genesis.rs`, conservation tested) |
| 5 | Deploys go to a random validator | `gaze::choose_validators`, redrawn at every start and after every resize; a validator is removed from F1R3Gaze's setting (and Embers moved off it) before it withdraws (`SupOps::evacuate`). ign1t10n's own deploys also go to a random active validator other than the one concerned (`Supervisor::random_active`). With G4, the whole list is written |
| 6 | Timings tuned for a laptop | `nodeconf::timing`, profile `laptop-v1`: casper loop 1.0 s at N = 2 rising to 3.0 s at N = 10 (upstream 750 ms); heartbeat on, maximum LFB age 15 s (N ≤ 4) or 30 s; discovery lookup 60 s. Recorded in `shard.toml`; T1 recalibrates and bumps the name |
| 7 | Refuse HTTP requests with a foreign `Origin` | `api-server.reject-foreign-origin = true` in `common.conf`; needs node work package **N5** (see `node-work-packages.md`). F1R3Gaze and Embers send no `Origin`, so they are unaffected |
| 8 | Embers is an option on the installation configuration panel, default on | `provision::Choices::embers` (default true), first-run options and Configure panel (`ui::appkit`), `ctl embers on|off`, `embers.rs` |

## Layout

```
src/
  main.rs         menu bar | supervise | ctl
  paths.rs        locations (all overridable by environment, for tests)
  manifest.rs     shard.toml, schema 3
  amounts.rs      genesis amounts (Decision 4)
  ports.rs        the port plan and fallback allocation
  keys.rs         secp256k1 keys; PKCS#8 PBES2 PBKDF2-SHA256/AES-256-CBC PEMs
  secrets.rs      Keychain (macOS) / 0600 file (tests, Linux)
  genesis.rs      bonds.txt, wallets.txt
  nodeconf.rs     common.conf + per-node files; timing profile (Decision 6)
  gaze.rs         F1R3Gaze settings block / settings.d; wallet discovery; random validator (Decision 5)
  embers.rs       Embers secrets and environment (Decision 8)
  api.rs          node HTTP client (deploy signing via gaze-shard)
  rho.rs          transfer, bond, withdraw, active-validators terms
  admin.rs        signing, submission, finalisation waits
  procs.rs        argument vectors, environment, rotating output, ordered kill
  control.rs      control-socket protocol, peer-uid check
  resize.rs       the pure resize engine (per-slot state machine)
  supervisor.rs   start/genesis/stop, health, restarts, resizes, socket server
  provision.rs    stages S0..S9, reprovision, configure_embers
  lifecycle.rs    compat, archive, uninstall
  logging.rs      rotating logs
  platform/       macos.rs (Keychain, SMAppService, IOPM, open) | generic.rs
  ui/             mod.rs (tested model) | appkit.rs (menu bar, windows)
examples/fake_node.rs   stand-in node/Embers for the end-to-end test
tests/e2e.rs            provision → genesis → crash → grow → shrink → uninstall
packaging/macos/        Info.plist, launch agent, entitlements, Distribution.xml, postinstall, uninstall.sh, build.sh
.github/workflows/      ci.yml (Linux + macOS), release.yml (node, embers, gaze, assemble, smoke, publish)
```

## What is verified, and what is not

Verified in the authoring environment (Linux, Rust 1.91):

* The crate builds without warnings; 26 unit tests pass, including OpenSSL
  (`openssl pkey`) decrypting the PEMs ign1t10n writes and seeing the same
  public key (Obligation "PEM compatibility").
* The end-to-end test passes: headless provisioning, genesis, F1R3Gaze
  settings, a SIGKILLed validator restarted, bounds (11 refused), growing
  2 → 3 and shrinking 3 → 2 (leave-before-stop checked, slot archived), and
  uninstall. It found and fixed two defects: a new slot's configuration was
  not rendered, and a validator that failed to join was restarted until the
  whole shard failed (it is now stopped and the resize waits for Retry/Undo).

Not verified here:

* **`platform/macos.rs` and `ui/appkit.rs` have not been compiled.** There is
  no Apple target in the authoring environment. They are written against
  objc2 0.5.2 / objc2-app-kit 0.2.2 / security-framework 2.11; the `macos` CI
  job is the first compiler to see them and will likely need small API
  fixes (method names, `unsafe` requirements, feature flags).
* Nothing has run against a real `f1r3node` (N3). To confirm there: the shape
  of `GET /api/blocks/0/0` (genesis hash is found by searching for
  `blockHash`), the rendering of `getActiveValidators` in explore-deploy
  answers (`rho::mentions_key` searches the JSON for the key's hex), that
  HOCON `include "common.conf"` resolves relative to the including file, the
  bond stake unit (1,000, as in `bonds.txt`), and Embers against a loopback
  shard with the same shard serving as its "mainnet" and "testnet".
* Footprint figures (700 MB memory and 2 GB disk per node) and the
  fault-tolerance table in `ui::TOLERANCE` are placeholders until T1.

## Deviations from the spec, and why

* **Templates are compiled in** (`nodeconf.rs`) rather than shipped as
  `Resources/templates/*.in`: one source, unit-tested rendering.
* **Notifications use `osascript`** so the launch agent can post them without
  a notification entitlement. UNUserNotificationCenter can replace it.
* **`Stop shard` keeps the supervisor running** (so Start works from the menu);
  only uninstall/`Shutdown` make it exit 0.
* **Bundling Embers** adds a funded service key per Embers network (both
  point at the local shard) and seven more secrets, kept in one Keychain item
  `embers.secrets`.
* **Before G1**, the F1R3Gaze wallet is found by parsing `f1r3gaze wallet list`
  (`* ADDRESS LABEL`) and created with `wallet new "Local shard"`.
* **The release workflow's node build asks for `crypto/vendored-openssl`**,
  which is work package N2 and does not yet exist in the pinned node; that
  job fails until N2 lands.

## Commands

```
ign1t10n ctl provision --non-interactive [--validators N] [--no-embers] [--no-gaze] [--open]
ign1t10n ctl status [--json] | start | stop | restart
ign1t10n ctl resize N [--new-shard] | retry | undo
ign1t10n ctl fund ADDRESS F1R3
ign1t10n ctl embers on|off | gaze on|off
ign1t10n ctl reallocate NODE | env | logs [NODE]
ign1t10n ctl reset [--validators N] --yes | uninstall --yes [--keep-archive]
```

Tests: `cargo test -- --test-threads=1`.
