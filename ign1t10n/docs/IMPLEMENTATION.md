# ign1t10n: implementation notes

This is the Rust implementation of the macOS installer described in
*ign1t10n: macOS Installer Specification*, **version 0.5** (bootstrap, 2–10
validators, observer, in-place resizing, optional Embers, and F1R3Games
served to any web browser, with F1R3Pix, F1R3Beat and F1R3Ink playable and
F1R3Ink's relay run in the portal). `docs/ign1t10n-macos-installer-spec.tex`
is v0.5.

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

## F1R3Games (spec v0.4, 7 October 2026)

ign1t10n installs F1R3Games and supervises its portal, `f1r3games-service`,
as one more process. The open decisions of v0.4 are implemented as proposed
(each is a setting, so a different answer changes a default, not the design):

| # | Decision | As implemented |
|---|----------|----------------|
| 9 | Port block | 40700 (portal) and 40701.. (one per game, in F1R3Games' order: pix, beat, ink, sidechat, skein); else the first free decade from 41700. `ports::allocate_games` |
| 10 | The local Cooperative | A key ign1t10n generates and keeps in `games.secrets`; it signs only `games.register`, in the registration job |
| 11 | Which games are registered | Those with a bundled web client (F1R3Pix, F1R3Beat); all five environments are installed. `games::bundle`, `games_sup::games_register` |
| 12 | Origins | One per game: the portal process listens on each game's port too (F1R3Games F2) |
| 13 | Opening the browser | At the end of first run, the portal in the default browser (`platform::open_url`), as well as F1R3Gaze |
| 14 | Faucet amount | 100 F1R3 per new portal key; `ctl games faucet N` |
| 15 | F1R3Beat breeder | Off; `ctl games breeder on` runs it daily with the headless CLI |
| 16 | The CLI | Bundled as `Helpers/f1r3games`; `ctl games env` prints `F1R3GAMES_SERVICE` |
| 17 | Default | On for new installs (`--no-games` to decline). A schema-3 manifest reads as off: after upgrading an existing install, turn it on with `ctl games on` or the Configure panel |

Where it lives:

* `games.rs`: `games.secrets` (service, environment, token, Cooperative,
  breeder and five game keys; kept across resets), the bundle's
  `Resources/f1r3games/games.toml`, G1/G2 (`prepare`), the portal's
  configuration (`render_config`, keys never in it), least-privilege
  environments (`env` with `Grant`), the job runner (`run_job`, logged to
  `games-job.log`), upgrade detection (`pending`).
* `supervisor/games_sup.rs`: G3..G6 as jobs of `f1r3games-service`
  (`bootstrap --wait`, `games-install --only --wait`, `status`,
  `games-manifests --entry ID=BASE`, `register-games`), the portal process
  (`Role::Portal`), its readiness (`/api/ready?games=…`) and health, moving
  it off a withdrawing validator, `ctl games on|off|reinstall|update|move|
  breeder|faucet`, and the breeder.
* `provision.rs`: the first-run choices (`games`, `games_open`) and stage
  `Games` (waits for G1..G6, then G7: opens the portal).
* `manifest.rs`: schema 4, `[options] games`, `[games]` and `[[games.game]]`.
* `examples/fake_node.rs`: enough registry for the real service's jobs
  (environment versions by URI, `games.register` parsed from the term).

### What is verified, and what is not

Verified here (Linux, Rust 1.97):

* 42 unit tests, including the new `games`, `manifest`, `ports` and `ui` tests.
* `tests/e2e.rs::f1r3games_installs_serves_survives_and_resets`, against
  the **real** `f1r3games-service` (built from F1R3Games with the F1..F7
  patches) and the stand-in node: G1..G7 at first run with the portal
  opened; the shell at `localhost` only (308 from `127.0.0.1`, 421 for a
  foreign Host); `/api/ready`; F1R3Pix's manifest entry at its own origin;
  that origin serving only F1R3Pix, with `frame-ancestors`; no key on any
  command line or in the configuration; a SIGKILLed portal restarted with
  the shard untouched; off freeing the ports and on restoring the same
  origin with no registration; a reset keeping the environment URI and
  registering again on the new chain. It found one defect, fixed: first-run
  provisioning saved an older copy of the manifest over the supervisor's
  F1R3Games progress, so the next start re-ran every stage.

Not verified here:

* `ui/appkit.rs` (the F1R3Games checkbox, menu items, update dialog) and
  `platform::macos::open_url` are not compiled: there is no Apple target in
  this environment. The `macos` CI job compiles them.
* Nothing has run against a real node: the environments' behaviour on a
  shard is F1R3Games' Obligation "The environments behave on a node".
* The breeder (`run_breeder`) has not run: the stand-in node cannot answer
  F1R3Beat's reads. `ctl games move` and `ctl games update` are exercised
  only through the code paths the e2e shares with them.
* Browsers: passkeys, IndexedDB and the game frames at `localhost` need the
  Obligation "Browsers" matrix (Safari, Chrome, Firefox).

### CI and release

* `versions.toml [f1r3games]` pins the revision, the clients and the
  environment versions. Since v0.5 it names `d2381c6` (branch
  `f1r3ink-impl`), which carries F1..F7 in tree and F8 (the relay); re-pin
  to `main` once that branch is merged.
* `packaging/macos/make-games.sh` builds the service, the CLI, the shell and
  the listed clients into `games-out/`; `build.sh` takes it as a fifth
  argument, signs the two new helpers and gates them (arm64, system libraries
  only, and none of the private keys committed in F1R3Games'
  `examples/local-shard`).

## F1R3Ink and its relay (spec v0.5, 9 October 2026)

F1R3Games at `d2381c6` adds F1R3Ink's web client (gallery renderers `round`
and `flag`) and, in `f1r3games-service`, F1R3Ink's relay: `POST
/api/relay/f1r3ink`, which takes a player's signed request, queues an
anonymous ink, and every three blocks deploys the window's inks as one
shuffled `f1r3ink.relayInk`, signed and paid for by the relay key. The
environment accepts `relayInk` and `relayReveal` only from the key that
F1R3Ink's own key named with `setRelay`. ign1t10n now:

| What | Where |
|------|-------|
| Bundles F1R3Ink's client at `games/f1r3ink/` (port base+3, reserved since v0.4), and checks each declared gallery kind has its renderer | `versions.toml` (`clients`), `make-games.sh` |
| Generates a relay key and a 32-byte handle secret in `games.secrets` (created on first use in a v0.4 install; kept across resets, like every F1R3Games key) | `games::GamesSecrets`, `load_or_create` |
| Runs the relay in the portal: `[relay]` in `games/f1r3games.toml` (`base_url = <portal>/api/relay`, three-block window, 30 requests per player per hour); `F1R3GAMES_RELAY_KEY` and `_SECRET` only in the portal's environment, and only when the file the portal will read enables the relay (so a setting saved while the file could not be rendered never starts a portal without the keys it asks for) | `games::render_config(.., relay)`, `games::config_runs_relay`, `Grant::relay`, `Supervisor::spawn_node` |
| Gives jobs a configuration without `[relay]`, `games/f1r3games-jobs.toml`: the service loads the relay's keys for every subcommand whenever its configuration enables the relay, so a job reading the portal's file would need them | `Paths::games_jobs_conf`, `games_sup::job` |
| Funds the relay (1,000 F1R3, topped up when below 100) and F1R3Ink's key (10 F1R3, it signs `setRelay`); a funding failure is logged and retried, never fails the install | `games_sup::fund_relay`, `relay_tick` |
| Registers F1R3Ink with `--relay-base <portal>/api/relay`, so its manifest names `<portal>/api/relay/f1r3ink` | `games_sup::games_register` |
| G7: names the relay with `f1r3games -y ink set-relay <address>` under F1R3Ink's key, waits for finalisation, and records the F1R3Ink environment version it was named for (`relay_named`): a reset or a new F1R3Ink environment names it again. A failure degrades F1R3Games (the games play; anonymous ink waits) and is retried every 15 minutes | `games_sup::games_name_relay`, `relay_tick`, `games_state` |
| Replaces a game's environment without asking when its version rose but it was never registered here (no instance can exist): F1R3Ink 1 → 2 on a v0.4 install | `games::silent_upgrades` |
| `ctl games relay on|off`; `ctl games open [GAME [launch|gallery]]`; the menu's "New <game> game" and "<game> gallery" items for every registered game; the relay in `ctl games status` | `main.rs`, `ui::menu`, `ui::portal_path`, `control::RelayReport` |
| Funds F1R3Beat's key before it signs `setBreeder` (a v0.4 defect: that deploy had nothing to pay its phlo with) | `games_sup::run_breeder` |

Decisions taken here (spec v0.5 §19.4, Decisions 18–23):

* **The relay is on by default** and runs inside the portal process, as
  F1R3Games implements it; there is no separate relay process. The local
  stand-in for the Cooperative runs it, as the F1R3Ink design (D10) has the
  Cooperative run the real one.
* **F1R3Ink's manifest always names the relay's URL**, even with the relay
  off; off, the portal answers 404 ("this portal runs no relay"), and a
  round launched with anonymous ink cannot use it. Reason: `register-games`
  compares a manifest's id, entry, environment URI, capabilities, renderers
  and template hashes, but not `relay`, so a manifest differing only in its
  relay is "current" and would not be registered again (F1R3Games work
  package F9 proposes comparing it).
* **F1R3Ink's environment is version 2.** The v0.4 bundle recorded 1 for an
  environment body that F1R3Ink design v1 rewrote; installs that installed
  version 1 never registered F1R3Ink, so it is replaced silently.

Verified here (Linux, Rust 1.97, F1R3Games `d2381c6` built from source):
the unit tests (44) and `tests/e2e.rs` against the real service and CLI and
the stand-in node: a v0.4 bundle provisioned (no relay, F1R3Ink an
unregistered v1 environment), then a v0.5 bundle at restart: F1R3Ink
replaced at v2 with nothing waiting for consent, registered at its own
origin with the relay URL in its manifest, its `flag` renderer served with
`frame-ancestors`, `[relay]` in the portal's file and not the jobs', the
relay named exactly once (`setRelay` seen by the node), a malformed relay
request refused with 400 (not 404: the relay runs); `ctl games relay off` registering
nothing and the relay answering 404; on again without naming it again; a
reset naming it on the new chain. `make-games.sh` was run against
`d2381c6` (host target): the service, the CLI, the shell, and the three
clients with their tests, F1R3Ink's renderers present.

An independent review of the change found, and these were fixed: the
portal's relay keys were granted from the setting rather than from the file
it reads (a crash restart after a busy `relay off` could loop); a failed
quarter-hourly relay check left F1R3Games `Failed`; a relay funding failure
failed the whole install; an unnamed relay (Degraded) stopped the breeder;
the status reported the relay running while the portal was down; and
`make-games.sh` passed silently when it found no gallery kinds.

Not verified here: a relayed ink end to end (it needs a real node: the
stand-in cannot answer `f1r3ink.players`), and the new menu action in
`ui/appkit.rs` (no Apple target here). `relay_named` records a finalised
`setRelay`, not a read-back of the environment's relay (no global read exists;
F10 would provide one).

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
  games.rs        F1R3Games secrets, bundle, portal configuration, jobs (spec v0.4)
  api.rs          node HTTP client (deploy signing via gaze-shard)
  rho.rs          transfer, bond, withdraw, active-validators terms
  admin.rs        signing, submission, finalisation waits
  procs.rs        argument vectors, environment, rotating output, ordered kill
  control.rs      control-socket protocol, peer-uid check
  resize.rs       the pure resize engine (per-slot state machine)
  supervisor.rs   start/genesis/stop, health, restarts, resizes, socket server
  supervisor/games_sup.rs  F1R3Games stages G1..G6, the portal, update, move, breeder
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
* **ign1t10n carries F1R3Gaze.app** in `Contents/Resources` as well as
  installing it through the package. S0 installs that copy (to
  `/Applications`, else `~/Applications`) when F1R3Gaze is missing, so a
  copied app or a deleted F1R3Gaze does not stop setup. For development,
  `IGN1T10N_GAZE_BIN` names a `f1r3gaze` executable instead.
* **Ports are free only if nothing answers.** On macOS, binding
  `127.0.0.1:p` succeeds while another program (Docker) listens on `*:p`, so
  `ports::port_free` also tries to connect.
* **The genesis ceremony waits for N0 − 1 approvals**, not N0: the node
  requires strictly fewer approvals than genesis validators (found on the
  first real-node run; the node's Docker shard uses 2 of 3).
* **Ports go on the command line.** The node's port options have default
  values (`--protocol-port 40400`, `--api-port-http 40403`, …) that override
  the configuration file, so ign1t10n passes all six ports explicitly
  (found on the first real multi-node run). The stand-in node now does the
  same, so the end-to-end test would catch a regression.
* **The network id goes on the command line too.** `protocol-client.network-id`
  defaults to a reference to the server's id that is resolved before
  ign1t10n's file is read, so outgoing messages said "testnet" and every peer
  refused them. `--network-id` sets both; `common.conf` sets both as well.
* **The bootstrap's node id comes from its output.** During the genesis
  ceremony a bootstrap waits for its first connection and cannot answer
  `/api/status`; the supervisor reads the id it prints at startup.
* **Node logs go to standard output** (`logging.sink = "stdout"`), which the
  supervisor writes to the rotating `<node>.stdout.log`.
* **Before G1**, the F1R3Gaze wallet is found by parsing `f1r3gaze wallet list`
  (`* ADDRESS LABEL`) and created with `wallet new "Local shard"`.
* **The release workflow's node build asks for `crypto/vendored-openssl`**,
  which is work package N2 and does not yet exist in the pinned node; that
  job fails until N2 lands.

## Commands

```
ign1t10n ctl provision --non-interactive [--validators N] [--no-embers] [--no-gaze] [--no-games] [--no-games-open] [--open]
ign1t10n ctl games status [--json] | on | off | open [GAME [launch|gallery]] | env | reinstall | update --yes | move --yes
ign1t10n ctl games breeder on|off | relay on|off | faucet F1R3
ign1t10n ctl status [--json] | start | stop | restart
ign1t10n ctl resize N [--new-shard] | retry | undo
ign1t10n ctl fund ADDRESS F1R3
ign1t10n ctl embers on|off | gaze on|off
ign1t10n ctl reallocate NODE | env | logs [NODE]
ign1t10n ctl reset [--validators N] --yes | uninstall --yes [--keep-archive]
```

Tests: `cargo test -- --test-threads=1`; with
`IGN1T10N_TEST_GAMES_BIN=<F1R3Games>/Portal/f1r3games-portal/target/debug/f1r3games-service`
(and the `f1r3games` CLI built beside it: `cargo build -p f1r3games-service
-p f1r3games-cli`) the F1R3Games end-to-end test runs too (it is skipped
otherwise).

## Known issues (from manual testing)

* **No feedback between the installer and the setup window.** After the
  installer's Close, nothing visible happens until ign1t10n's window appears
  (the installer's last step first stops the old supervisor). Needs a visible
  sign of progress, e.g. open the app first and let it show "Restarting the
  shard supervisor…".
* **Moving every clashing node at once.** When a later start finds the
  shard's ports taken (e.g. a Docker shard came back), the menu offers to
  move one node at a time; it should offer to move all of them.
* **The supervisor is a classic per-user launch agent, not an SMAppService
  agent** (deviation from spec §5.2). ign1t10n writes
  `~/Library/LaunchAgents/io.f1r3fly.ign1t10n.supervisor.plist` with the
  supervisor's absolute path and loads it with `launchctl bootstrap`,
  rewriting and reloading it whenever the path or contents differ, and
  removes any SMAppService registration from earlier versions. Reason: after
  a reinstall, launchd kept a stale background-task entry id for the
  SMAppService agent, could not resolve its app-relative program path
  (`copy_bundle_path … Invalid or missing Program/ProgramArguments`), and never
  started the supervisor again; re-registering did not repair it. macOS 13
  remains the minimum. The agent still appears under Login Items.
* **Background item after reinstalling an ad-hoc build** (superseded by the
  above; kept for the record). launchd refuses to
  spawn the supervisor (`spawn failed`, `EX_CONFIG`) because the registration
  belongs to the previous build's signature. The app now re-registers when
  launchd does not run it, and ad-hoc builds get a fixed designated
  requirement. Developer ID builds keep a stable identity and should not
  hit this; to be confirmed when signing is set up.
* **Embers' contracts do not parse on the pinned node.** Embers `28e50d4`
  deploys its "agents" environment; node `fce422a` rejects the term with
  parse errors around `contract toAgentHeader(@(version, agent), ret)` and
  `recordDeploy` in `packages/embers/templates/agents/init.rho` (an
  identifier, likely `agent`, and the method call on it). Suspected: a word
  that became reserved in the node's grammar. Needs a compatible version
  pair in versions.toml, and the release smoke job should deploy Embers'
  contracts so a mismatch fails the build. Workaround: `ctl embers off`.
* **Health check:** `ign1t10n ctl status` gives each node's readiness, last
  finalised block and peers; a funded test deploy (`ctl fund`) exercises
  signing, deploy, proposal and finalisation end to end.

## Requests to other projects

* **F1R3Gaze, G5: a shard health "app".** A page in F1R3Gaze that reports a
  shard's health, defaulting to the shard F1R3Gaze is configured for (the
  local shard when ign1t10n set it up) but able to point at any reachable
  shard by its observer and validator URLs. It should show what
  `ign1t10n ctl status` and the health check above show: each node's
  readiness, last finalised block and peers; whether finalisation is
  advancing; a balance read through the observer; and, on request, a small
  test deploy whose finalisation it follows end to end. It reads only the
  nodes' public HTTP APIs, so it needs nothing from ign1t10n and works
  against any shard.
