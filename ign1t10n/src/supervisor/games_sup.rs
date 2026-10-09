//! F1R3Games under the supervisor (spec v0.5 §10): stages G1..G7, the
//! portal process (with F1R3Ink's relay in it), its health, retargeting it
//! off a withdrawing validator, upgrades that need consent and those that do
//! not, moving it to new origins, the relay's naming and funding, and the
//! F1R3Beat breeder. G8 (opening the browser) belongs to first-run
//! provisioning.
//!
//! Every on-chain step is a job of `f1r3games-service` that probes the chain
//! before it deploys and waits until its work reads back, so each stage is
//! idempotent: an interrupted stage simply runs again.

use super::Supervisor;
use crate::amounts::dust;
use crate::api::Api;
use crate::control::{GameReport, GamesReport, GamesState, RelayReport, ShardState};
use crate::games::{self, Grant};
use crate::manifest::{Manifest, RELAY_GAME};
use crate::ports::{self, Block};
use crate::procs::Role;
use crate::{error, info, warn};
use rand::seq::SliceRandom;
use std::time::{Duration, Instant};

const JOB_TIMEOUT: Duration = Duration::from_secs(games::JOB_WAIT_SECS + 120);
const BREEDER_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How often the relay is checked: named for F1R3Ink's environment, funded.
const RELAY_EVERY: Duration = Duration::from_secs(15 * 60);

pub(super) fn report(m: &Manifest, state: &GamesState) -> Option<crate::control::GamesReport> {
    let g = m.games.as_ref()?;
    if !m.options.games && !g.stages.secrets {
        return None;
    }
    Some(GamesReport {
        state: state.clone(),
        url: g.url(),
        games: g
            .game
            .iter()
            .map(|x| GameReport { id: x.id.clone(), origin: x.port.filter(|_| x.client).map(games::entry_base), env_version: x.env_version, client: x.client, registered: x.registered })
            .collect(),
        faucet_f1r3: g.faucet_f1r3,
        breeder: g.breeder,
        open_at_first_run: m.options.games_open_at_first_run,
        pending_update: g.pending_update.clone(),
        last_epoch: g.last_epoch.clone(),
        relay: RelayReport {
            on: g.relay,
            running: g.relay_on(),
            address: g.relay_address.clone(),
            url: if g.relay_on() { format!("{}/{RELAY_GAME}", g.relay_base()) } else { String::new() },
            named: g.relay_current(),
        },
    })
}

impl Supervisor {
    fn set_games(&self, s: GamesState) {
        let mut g = self.lock();
        if g.games != s {
            info!("F1R3Games: {}", s.label());
        }
        g.games = s;
    }

    fn games_stage(&self, what: &str) {
        self.set_games(GamesState::Installing(what.into()));
    }

    /// After the shard stops: F1R3Games waits for it (or is off).
    pub(super) fn games_waiting(&self) {
        let on = self.manifest().map(|m| m.options.games).unwrap_or(false);
        self.set_games(if on { GamesState::Waiting } else { GamesState::Off });
    }

    /// Run `f` on its own thread unless the shard or F1R3Games is busy.
    pub(super) fn games_job(&self, what: &'static str, f: impl FnOnce(&Supervisor) -> Result<(), String> + Send + 'static) -> bool {
        {
            let mut g = self.lock();
            if g.busy || g.games_busy {
                return false;
            }
            g.games_busy = true;
        }
        let s = self.clone();
        std::thread::spawn(move || {
            let _awake = crate::platform::prevent_idle_sleep(what);
            if let Err(e) = f(&s) {
                error!("F1R3Games {what}: {e}");
                s.set_games(GamesState::Failed(e.clone()));
                crate::platform::notify("F1R3Games", &format!("{what} failed: {e}"));
            }
            s.lock().games_busy = false;
        });
        true
    }

    /// Called by `start` once the shard runs. Never fails the start.
    pub(super) fn start_games(&self) {
        let Ok(m) = self.manifest() else { return };
        if !m.options.games {
            self.set_games(GamesState::Off);
            return;
        }
        self.lock().games_busy = true;
        let r = self.games_install(false);
        self.lock().games_busy = false;
        if let Err(e) = r {
            error!("F1R3Games: {e}");
            self.set_games(GamesState::Failed(e.clone()));
            crate::platform::notify("F1R3Games", &e);
        }
    }

    fn job(&self, args: &[String], grant: Grant) -> Result<String, String> {
        let sec = games::load_or_create(&*self.secrets)?;
        // The jobs' configuration has no `[relay]`, so no job needs the relay's keys.
        let mut a = vec!["-c".to_string(), self.paths.games_jobs_conf().display().to_string()];
        a.extend(args.iter().cloned());
        games::run_job(&self.paths, &self.paths.games_bin, &a, &games::env(&sec, grant), JOB_TIMEOUT)
    }

    fn cli(&self, key_hex: &str, args: &[&str]) -> Result<String, String> {
        let g = self.manifest()?.games.ok_or("F1R3Games is not configured")?;
        if !self.paths.games_cli.exists() {
            return Err(format!("this build carries no f1r3games CLI ({})", self.paths.games_cli.display()));
        }
        let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        games::run_job(&self.paths, &self.paths.games_cli, &a, &games::cli_env(&self.paths, key_hex, &g.url()), JOB_TIMEOUT)
    }

    fn fund_if_short(&self, address: &str, amount: i64) -> Result<(), String> {
        let obs = self.observer()?;
        if obs.balance(address).unwrap_or(0) >= amount {
            return Ok(());
        }
        let id = self.faucet_transfer(address, amount)?;
        info!("funded {address} for F1R3Games ({id})");
        Ok(())
    }

    /// Render the portal's configuration, its validators drawn afresh and
    /// `exclude` left out (a validator about to withdraw).
    fn games_render(&self, exclude: Option<u8>, env_version: i64) -> Result<(), String> {
        let m = self.manifest()?;
        let g = m.games.clone().ok_or("F1R3Games is not configured")?;
        let mut vs: Vec<(u8, String)> = m.active().into_iter().filter(|v| Some(v.slot) != exclude).map(|v| (v.slot, Block(v.base_port).http_url())).collect();
        if vs.is_empty() {
            return Err("no active validator for the portal".into());
        }
        vs.shuffle(&mut rand::thread_rng());
        let urls: Vec<String> = vs.iter().map(|(_, u)| u.clone()).collect();
        let observer = Block(m.observer.base_port).http_url();
        std::fs::create_dir_all(self.paths.games()).map_err(|e| e.to_string())?;
        for (file, relay) in [(self.paths.games_conf(), g.relay_on()), (self.paths.games_jobs_conf(), false)] {
            let text = games::render_config(&self.paths, &g, &urls, &observer, &m.shard.id, env_version, relay);
            crate::paths::write_atomic(&file, text.as_bytes(), 0o600).map_err(|e| format!("{}: {e}", file.display()))?;
        }
        let slot = vs[0].0;
        self.update(|m| {
            if let Some(x) = m.games.as_mut() {
                x.validator_slot = Some(slot);
            }
            Ok(())
        })?;
        Ok(())
    }

    fn configured_version(m: &Manifest, b: &games::Bundle) -> i64 {
        match &m.games {
            Some(g) if g.stages.portal_env && g.env_version > 0 => g.env_version,
            _ => b.env_version,
        }
    }

    /// G1..G6 and the portal. `force` re-runs G3..G6 (Reinstall).
    fn games_install(&self, force: bool) -> Result<(), String> {
        let b = games::bundle(&self.paths)?;
        let sec = games::load_or_create(&*self.secrets)?;
        // G1, G2.
        self.games_stage("keys and ports");
        self.update(|m| games::prepare(m, &sec, &b))?;
        let g = self.manifest()?.games.unwrap();
        if !self.running(&Role::Portal) {
            for p in g.ports() {
                if !ports::port_free_dual(p, g.ipv6) {
                    let who = ports::holder(p).unwrap_or_else(|| "another program".into());
                    // Never moved silently: browser keystores live at this origin.
                    return Err(format!("port {p} is in use by {who}; stop it (F1R3Games keeps its address, where your browser keystores are), or move F1R3Games to a new address"));
                }
            }
        }
        // G3.
        if force || !g.stages.funded {
            self.games_stage("funding the portal");
            self.fund_if_short(&g.service_address, dust(games::SERVICE_F1R3))?;
            self.fund_if_short(&g.coop_address, dust(games::COOP_F1R3))?;
            if g.breeder {
                self.fund_if_short(&g.breeder_address, dust(games::BREEDER_F1R3))?;
            }
            if g.relay_on() {
                self.fund_relay(&sec)?;
            }
            self.update(|m| {
                m.games.as_mut().unwrap().stages.funded = true;
                Ok(())
            })?;
        } else if g.relay_on() {
            // The relay switched on, or F1R3Ink arrived, after G3.
            self.fund_relay(&sec)?;
        }
        let version = Self::configured_version(&self.manifest()?, &b);
        self.games_render(None, version)?;
        // G4.
        let g = self.manifest()?.games.unwrap();
        if force || !g.stages.portal_env {
            self.games_stage("installing the portal environment");
            self.job(&["bootstrap".into(), "--wait".into(), games::JOB_WAIT_SECS.to_string()], Grant { portal: true, ..Default::default() })?;
            let uri = self.job(&["env-uri".into()], Grant { portal: true, ..Default::default() })?.trim().to_string();
            self.update(|m| {
                let x = m.games.as_mut().unwrap();
                x.env_uri = uri;
                x.env_version = version;
                x.stages.portal_env = true;
                Ok(())
            })?;
        }
        // G5.
        let g = self.manifest()?.games.unwrap();
        if force || !g.stages.game_envs {
            for bg in &b.games {
                let v = g.game(&bg.id).map(|x| x.env_version).filter(|v| *v > 0).unwrap_or(bg.env_version);
                self.games_stage(&format!("installing the {} environment", bg.id));
                self.job(
                    &["games-install".into(), "--version".into(), v.to_string(), "--only".into(), bg.id.clone(), "--wait".into(), games::JOB_WAIT_SECS.to_string()],
                    Grant { portal: true, game_keys: true, ..Default::default() },
                )?;
                self.update(|m| {
                    if let Some(x) = m.games.as_mut().unwrap().game_mut(&bg.id) {
                        x.env_version = v;
                    }
                    Ok(())
                })?;
            }
            self.record_status()?;
            self.update(|m| {
                m.games.as_mut().unwrap().stages.game_envs = true;
                Ok(())
            })?;
        }
        // A game never registered here has no instances: its environment is
        // replaced at the bundle's version without asking (spec v0.5 §10.9).
        for (id, v) in games::silent_upgrades(&self.manifest()?.games.unwrap(), &b) {
            self.games_stage(&format!("updating the {id} environment (never played here)"));
            self.job(
                &["games-install".into(), "--version".into(), v.to_string(), "--only".into(), id.clone(), "--wait".into(), games::JOB_WAIT_SECS.to_string()],
                Grant { portal: true, game_keys: true, ..Default::default() },
            )?;
            self.update(|m| {
                if let Some(x) = m.games.as_mut().unwrap().game_mut(&id) {
                    x.env_version = v;
                }
                Ok(())
            })?;
            info!("{id}: environment replaced at version {v}; it had never been registered");
        }
        // An update that would discard state waits for consent.
        let pending = games::pending(&self.manifest()?.games.unwrap(), &b);
        self.update(|m| {
            let x = m.games.as_mut().unwrap();
            x.pending_update = pending.clone();
            x.revision = if pending.is_none() { b.revision.clone() } else { x.revision.clone() };
            Ok(())
        })?;
        // G6.
        self.games_register(force)?;
        // The portal.
        self.games_stage("starting the portal");
        if !self.running(&Role::Portal) {
            self.spawn_node(Role::Portal)?;
        }
        self.wait_ready(&Role::Portal, Duration::from_secs(120))?;
        // G7: name the relay for F1R3Ink's environment. The games are playable
        // without it (only anonymous ink waits), so a failure degrades.
        let relay_err = self.games_name_relay(force).err();
        let g = self.manifest()?.games.unwrap();
        self.set_games(games_state(&g, relay_err.as_deref()));
        info!("F1R3Games at {}", g.url());
        Ok(())
    }

    /// Fund the relay's key (it pays every relayed ink) and F1R3Ink's own key
    /// (it signs `setRelay`).
    fn fund_relay(&self, sec: &games::GamesSecrets) -> Result<(), String> {
        self.fund_if_short(&sec.relay_address()?, dust(games::RELAY_F1R3))?;
        self.fund_if_short(&sec.game_address(RELAY_GAME)?, dust(games::GAME_KEY_F1R3))?;
        Ok(())
    }

    /// G7: `setRelay(relay address)`, signed with F1R3Ink's key through the
    /// headless CLI (F8), when the relay runs and is not yet named for
    /// F1R3Ink's current environment; waits until it is finalised.
    fn games_name_relay(&self, force: bool) -> Result<(), String> {
        let g = self.manifest()?.games.ok_or("F1R3Games is not installed")?;
        if !g.relay_on() || (g.relay_current() && !force) {
            return Ok(());
        }
        if !g.game(RELAY_GAME).is_some_and(|x| x.registered) {
            return Err("F1R3Ink is not registered, so its relay cannot be named".into());
        }
        self.games_stage("naming F1R3Ink's relay");
        let sec = games::load_or_create(&*self.secrets)?;
        self.fund_relay(&sec)?;
        let ink = sec.game_keys.get(RELAY_GAME).ok_or("no F1R3Ink key")?;
        let out = self.cli(ink, &["-y", "ink", "set-relay", &g.relay_address])?;
        let id = out
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()?["deployId"].as_str().map(str::to_string))
            .filter(|d| !d.is_empty())
            .ok_or_else(|| format!("no deploy id from ink set-relay: {}", out.trim()))?;
        let (_, target) = self.random_active(None)?;
        crate::admin::await_finalized(&target, &id, Duration::from_secs(games::JOB_WAIT_SECS))?;
        let v = g.game(RELAY_GAME).map(|x| x.env_version).unwrap_or(0);
        self.update(|m| {
            m.games.as_mut().unwrap().relay_named = Some(v);
            Ok(())
        })?;
        info!("F1R3Ink's relay {} named on chain (environment v{v}, deploy {id})", g.relay_address);
        Ok(())
    }

    /// Record each game environment's URI and version from `status`.
    fn record_status(&self) -> Result<(), String> {
        let out = self.job(&["status".into()], Grant { portal: true, game_keys: true, ..Default::default() })?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("status: {e}"))?;
        let uris = games::env_uris(&v);
        self.update(|m| {
            let g = m.games.as_mut().unwrap();
            for (id, (uri, ver)) in &uris {
                if let Some(x) = g.game_mut(id) {
                    x.env_uri = uri.clone();
                    if let Some(ver) = ver {
                        x.env_version = *ver;
                    }
                }
            }
            if let Some(u) = v["envUri"].as_str() {
                g.env_uri = u.to_string();
            }
            Ok(())
        })?;
        Ok(())
    }

    /// G6: write the manifests with each game's own origin, and register the
    /// ones that are new or changed with the local Cooperative's key.
    fn games_register(&self, force: bool) -> Result<(), String> {
        let g = self.manifest()?.games.unwrap();
        let served: Vec<(String, u16)> = g.served().iter().map(|x| (x.id.clone(), x.port.unwrap())).collect();
        if served.is_empty() {
            return self.update(|m| {
                m.games.as_mut().unwrap().stages.registered = true;
                Ok(())
            })
            .map(|_| ());
        }
        self.games_stage("registering the games");
        std::fs::create_dir_all(self.paths.games_manifests()).map_err(|e| e.to_string())?;
        let file = self.paths.games_manifests().join("manifests.json");
        let mut args = vec!["games-manifests".to_string(), "--keys".into(), self.paths.games().join("no-key-files").display().to_string(), "--out".into(), file.display().to_string()];
        for (id, port) in &served {
            args.extend(["--entry".into(), format!("{id}={}", games::entry_base(*port)), "--only".into(), id.clone()]);
        }
        // F1R3Ink's manifest names where its relay lives: the portal's
        // `/api/relay` (F8). It is named whether or not the relay runs, so
        // switching the relay neither re-registers F1R3Ink nor changes a hash
        // (and `register-games` does not compare `relay`; see F9). With the
        // relay off, the portal answers that it runs none.
        if served.iter().any(|(id, _)| id == RELAY_GAME) {
            args.extend(["--relay-base".into(), g.relay_base()]);
        }
        self.job(&args, Grant { game_keys: true, ..Default::default() })?;
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let hashes = games::manifest_entries(&text)?;
        let due: Vec<String> = hashes
            .iter()
            .filter(|(id, h)| force || g.game(id).map(|x| !x.registered || x.manifest_sha256.as_deref() != Some(h.as_str())).unwrap_or(true))
            .map(|(id, _)| id.clone())
            .collect();
        if !due.is_empty() {
            let mut a = vec!["register-games".to_string(), file.display().to_string(), "--wait".into(), games::JOB_WAIT_SECS.to_string()];
            for id in &due {
                a.extend(["--only".into(), id.clone()]);
            }
            let out = self.job(&a, Grant { portal: true, coop: true, ..Default::default() })?;
            info!("registration: {}", out.trim().replace('\n', "; "));
        }
        self.update(|m| {
            let x = m.games.as_mut().unwrap();
            for (id, h) in &hashes {
                if let Some(gm) = x.game_mut(id) {
                    gm.registered = true;
                    gm.manifest_sha256 = Some(h.clone());
                }
            }
            x.stages.registered = true;
            Ok(())
        })?;
        Ok(())
    }

    /// Restart the portal pointed at fresh validators, leaving `exclude` out.
    pub(super) fn games_retarget(&self, exclude: Option<u8>) -> Result<(), String> {
        let m = self.manifest()?;
        let version = m.games.as_ref().map(|g| g.env_version).unwrap_or(1);
        self.games_render(exclude, version)?;
        if self.running(&Role::Portal) {
            self.stop_role(&Role::Portal, Duration::from_secs(30));
            self.spawn_node(Role::Portal)?;
            self.wait_ready(&Role::Portal, Duration::from_secs(60))?;
        }
        Ok(())
    }

    pub(super) fn games_reinstall(&self) -> Result<(), String> {
        self.games_install(true)
    }

    /// Apply the waiting update (consent given): the portal environment at
    /// the bundle's version and each game whose environment version rose,
    /// then re-registration.
    pub(super) fn games_update(&self) -> Result<(), String> {
        let b = games::bundle(&self.paths)?;
        let g = self.manifest()?.games.ok_or("F1R3Games is not installed")?;
        if g.pending_update.is_none() {
            return Err("no update is waiting".into());
        }
        self.stop_role(&Role::Portal, Duration::from_secs(30));
        if b.env_version > g.env_version {
            self.games_render(None, b.env_version)?;
            self.games_stage("updating the portal environment");
            self.job(&["bootstrap".into(), "--wait".into(), games::JOB_WAIT_SECS.to_string()], Grant { portal: true, ..Default::default() })?;
            self.update(|m| {
                m.games.as_mut().unwrap().env_version = b.env_version;
                Ok(())
            })?;
        }
        for bg in &b.games {
            if g.game(&bg.id).map(|x| bg.env_version > x.env_version).unwrap_or(false) {
                self.games_stage(&format!("updating the {} environment", bg.id));
                self.job(
                    &["games-install".into(), "--version".into(), bg.env_version.to_string(), "--only".into(), bg.id.clone(), "--wait".into(), games::JOB_WAIT_SECS.to_string()],
                    Grant { portal: true, game_keys: true, ..Default::default() },
                )?;
                self.update(|m| {
                    m.games.as_mut().unwrap().game_mut(&bg.id).unwrap().env_version = bg.env_version;
                    Ok(())
                })?;
            }
        }
        self.update(|m| {
            m.games.as_mut().unwrap().pending_update = None;
            Ok(())
        })?;
        self.games_install(false)
    }

    /// New origins (consent given): new ports, re-registration under them.
    pub(super) fn games_move(&self) -> Result<(), String> {
        self.stop_role(&Role::Portal, Duration::from_secs(30));
        self.update(|m| {
            let g = m.games.as_mut().ok_or("F1R3Games is not installed")?;
            g.stages.ports = false;
            Ok(())
        })?;
        self.games_install(false)
    }

    pub(super) fn games_set(&self, on: Option<bool>, breeder: Option<bool>, faucet: Option<i64>, open: Option<bool>, relay: Option<bool>) -> Result<String, String> {
        let mut said = vec![];
        if let Some(r) = relay {
            self.update(|m| {
                m.games.as_mut().ok_or("F1R3Games is not installed")?.relay = r;
                Ok(())
            })?;
            // The portal's configuration and keys change: restart it through
            // the install path, which deploys only what is missing (funding
            // and naming the relay when it comes on).
            self.lock().relay_at = None;
            if m_games_running(self) {
                let before = self.lock().games.clone();
                self.games_stage("applying the relay setting");
                let ok = self.games_job("relay", |s| {
                    s.stop_role(&Role::Portal, Duration::from_secs(30));
                    s.games_install(false)
                });
                if !ok {
                    self.set_games(before);
                    return Err("busy; the relay setting is saved and takes effect at the next start".into());
                }
            }
            let g = self.manifest()?.games.unwrap();
            said.push(match (r, g.relay_on()) {
                (false, _) => "F1R3Ink's relay is off: anonymous ink is unavailable in new and running rounds".to_string(),
                (true, true) if g.relay_current() => "F1R3Ink's relay is on".to_string(),
                (true, true) => "F1R3Ink's relay is on; it is being named on the chain".to_string(),
                (true, false) => "F1R3Ink's relay will run once F1R3Ink's client is installed".to_string(),
            });
        }
        if let Some(f) = faucet {
            if !(1..=10_000).contains(&f) {
                return Err("the faucet amount is between 1 and 10,000 F1R3".into());
            }
            self.update(|m| {
                m.games.as_mut().ok_or("F1R3Games is not installed")?.faucet_f1r3 = f;
                Ok(())
            })?;
            if self.running(&Role::Portal) {
                self.games_retarget(None)?;
            }
            said.push(format!("new portal keys receive {f} F1R3"));
        }
        if let Some(b) = breeder {
            self.update(|m| {
                let g = m.games.as_mut().ok_or("F1R3Games is not installed")?;
                g.breeder = b;
                Ok(())
            })?;
            self.lock().breeder_at = None;
            said.push(format!("the F1R3Beat breeder is {}", if b { "on (daily)" } else { "off" }));
        }
        if let Some(o) = open {
            self.update(|m| {
                m.options.games_open_at_first_run = o;
                Ok(())
            })?;
        }
        match on {
            Some(true) => {
                self.update(|m| {
                    m.options.games = true;
                    Ok(())
                })?;
                if matches!(self.lock().state, ShardState::Running | ShardState::Degraded(_)) {
                    if !self.games_job("install", |s| s.games_install(false)) {
                        return Err("busy; F1R3Games will start with the shard".into());
                    }
                    said.push("installing F1R3Games".into());
                } else {
                    self.set_games(GamesState::Waiting);
                    said.push("F1R3Games will start with the shard".into());
                }
            }
            Some(false) => {
                self.stop_role(&Role::Portal, Duration::from_secs(30));
                self.update(|m| {
                    m.options.games = false;
                    Ok(())
                })?;
                self.set_games(GamesState::Off);
                said.push("F1R3Games is off; its environments, registrations and keys are kept".into());
            }
            None => {}
        }
        Ok(if said.is_empty() { "nothing changed".into() } else { said.join("; ") })
    }

    pub(super) fn games_health(&self) {
        let Ok(m) = self.manifest() else { return };
        let Some(g) = m.games.as_ref().filter(|_| m.options.games) else { return };
        let state = self.lock().games.clone();
        if !matches!(state, GamesState::Running | GamesState::Degraded(_)) || self.lock().games_busy {
            return;
        }
        let ok = self.running(&Role::Portal) && Api::new(g.url()).get_ok("/api/health").is_ok();
        let s = if ok { games_state(g, None) } else { GamesState::Degraded("the portal is not answering".into()) };
        self.set_games(s);
    }

    /// Every quarter of an hour: name the relay if it is not named for
    /// F1R3Ink's current environment (a failed G7, a reset elsewhere), and
    /// top up its key when it runs low.
    pub(super) fn relay_tick(&self) {
        let Ok(m) = self.manifest() else { return };
        let Some(g) = m.games.as_ref().filter(|g| m.options.games && g.relay_on()) else { return };
        {
            let x = self.lock();
            if !matches!(x.games, GamesState::Running | GamesState::Degraded(_)) || x.games_busy || x.busy || x.relay_at.is_some_and(|t| t.elapsed() < RELAY_EVERY) {
                return;
            }
        }
        self.lock().relay_at = Some(Instant::now());
        let named = g.relay_current();
        let address = g.relay_address.clone();
        self.games_job("relay", move |s| {
            if !named {
                if let Err(e) = s.games_name_relay(false) {
                    warn!("F1R3Ink's relay: {e}");
                }
            }
            // An unreadable balance is left for the next check.
            if let Ok(have) = s.observer()?.balance(&address) {
                if have < dust(games::RELAY_LOW_F1R3) {
                    let id = s.faucet_transfer(&address, dust(games::RELAY_F1R3) - have.max(0))?;
                    info!("topped up F1R3Ink's relay to {} ({id})", crate::amounts::show(dust(games::RELAY_F1R3)));
                }
            }
            let g = s.manifest()?.games.unwrap();
            s.set_games(games_state(&g, None));
            Ok(())
        });
    }

    /// Once a minute: start a breeder run when one is due.
    pub(super) fn breeder_tick(&self) {
        let Ok(m) = self.manifest() else { return };
        let Some(g) = m.games.as_ref().filter(|g| m.options.games && g.breeder) else { return };
        {
            let x = self.lock();
            if x.games != GamesState::Running || x.games_busy || x.busy || x.breeder_at.is_some_and(|t| t.elapsed() < BREEDER_EVERY) {
                return;
            }
        }
        let due = g
            .last_epoch
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| chrono::Utc::now().signed_duration_since(t) > chrono::Duration::hours(24))
            .unwrap_or(true);
        self.lock().breeder_at = Some(Instant::now());
        if due {
            self.games_job("breeder", |s| s.run_breeder());
        }
    }

    /// Name the breeder and open its nursery once, then run one epoch with
    /// the headless CLI (F1R3Games F6).
    fn run_breeder(&self) -> Result<(), String> {
        let sec = games::load_or_create(&*self.secrets)?;
        let g = self.manifest()?.games.ok_or("F1R3Games is not installed")?;
        self.fund_if_short(&g.breeder_address, dust(games::BREEDER_F1R3))?;
        let nursery = match g.nursery.clone() {
            Some(n) => n,
            None => {
                let beat = sec.game_keys.get("f1r3beat").ok_or("no F1R3Beat key")?;
                // F1R3Beat's key signs `setBreeder`, so it pays that deploy's phlo.
                self.fund_if_short(&sec.game_address("f1r3beat")?, dust(games::GAME_KEY_F1R3))?;
                self.cli(beat, &["-y", "beat", "set-breeder", &g.breeder_address])?;
                let out = self.cli(&sec.breeder_key, &["-y", "launch", "f1r3beat", "--visibility", "public", "--config", games::NURSERY_CONFIG])?;
                let id = out.lines().find_map(|l| l.strip_prefix("instance ")).map(|s| s.trim().to_string()).ok_or_else(|| format!("no instance id in: {out}"))?;
                self.update(|m| {
                    m.games.as_mut().unwrap().nursery = Some(id.clone());
                    Ok(())
                })?;
                id
            }
        };
        match self.cli(&sec.breeder_key, &["-y", "beat", "epoch", "--nursery", &nursery]) {
            Ok(out) => info!("breeder: {}", out.trim().replace('\n', "; ")),
            Err(e) => warn!("breeder: {e}"),
        }
        self.update(|m| {
            m.games.as_mut().unwrap().last_epoch = Some(crate::now_rfc3339());
            Ok(())
        })?;
        Ok(())
    }
}

/// F1R3Games' state once the portal answers: an update waiting, or the relay
/// not named, degrade it; otherwise it runs.
fn games_state(g: &crate::manifest::Games, relay_err: Option<&str>) -> GamesState {
    match (&g.pending_update, g.relay_on() && !g.relay_current()) {
        (Some(p), _) => GamesState::Degraded(format!("update waiting: {p}")),
        (None, true) => GamesState::Degraded(format!("F1R3Ink's relay is not named on the chain yet{}", relay_err.map(|e| format!(": {e}")).unwrap_or_default())),
        (None, false) => GamesState::Running,
    }
}

/// Whether F1R3Games is installed and its portal should be running now.
fn m_games_running(s: &Supervisor) -> bool {
    s.manifest().map(|m| m.options.games).unwrap_or(false) && matches!(s.lock().state, ShardState::Running | ShardState::Degraded(_)) && s.manifest().ok().and_then(|m| m.games).is_some_and(|g| g.stages.installed())
}
