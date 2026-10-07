//! `ign1t10n supervise`: the launch agent's program (spec §8). It owns every
//! process: ordered start (bootstrap, validators, observer, Embers, the
//! F1R3Games portal), the genesis ceremony, F1R3Games' stages G1..G7
//! (`games_sup`), health, restarts with back-off, ordered stop, resizes, and
//! the control socket.

mod games_sup;

use crate::admin;
use crate::amounts::*;
use crate::api::Api;
use crate::control::{self, GamesState, NodeReport, Report, Request, Response, ShardState};
use crate::keys::Key;
use crate::manifest::{Manifest, SlotState, Validator};
use crate::paths::Paths;
use crate::ports::{self, Block};
use crate::procs::{self, Proc, Role};
use crate::resize::{self, Ops};
use crate::secrets::{self, Secrets};
use crate::{error, info, warn};
use gaze_wallet::Address;
use rand::seq::SliceRandom;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

/// An upper bound on any single wait for a node to become ready, for test
/// harnesses (`IGN1T10N_START_TIMEOUT_SECS`); unset in production.
fn start_timeout_cap() -> Duration {
    Duration::from_secs(std::env::var("IGN1T10N_START_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(u64::MAX / 4))
}

const FAILS_ALLOWED: usize = 5;
const FAIL_WINDOW: Duration = Duration::from_secs(300);

#[derive(Default)]
struct Inner {
    manifest: Option<Manifest>,
    procs: BTreeMap<Role, Proc>,
    state: ShardState,
    /// The shard should be running (restart what exits).
    want: bool,
    /// A start, stop, resize or reallocation is in progress.
    busy: bool,
    exits: BTreeMap<Role, Vec<Instant>>,
    restart_at: BTreeMap<Role, (Instant, u32)>,
    health: BTreeMap<Role, (bool, i64, i64)>,
    lag_since: Option<Instant>,
    resize_msg: Option<String>,
    exposed: Vec<String>,
    shutdown: bool,
    /// F1R3Games' state, beside the shard's.
    games: GamesState,
    /// A G-stage, update, move or breeder run is in progress.
    games_busy: bool,
    /// When the breeder last ran (or was last tried).
    breeder_at: Option<Instant>,
}

#[derive(Clone)]
pub struct Supervisor {
    pub paths: Paths,
    secrets: Arc<dyn Secrets>,
    inner: Arc<Mutex<Inner>>,
}

impl Supervisor {
    pub fn new(paths: Paths) -> Supervisor {
        let secrets: Arc<dyn Secrets> = Arc::from(secrets::open(&paths));
        Supervisor { paths, secrets, inner: Arc::new(Mutex::new(Inner::default())) }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn manifest(&self) -> Result<Manifest, String> {
        self.lock().manifest.clone().ok_or_else(|| "no shard is provisioned yet".into())
    }

    /// Change and persist the manifest.
    fn update(&self, f: impl FnOnce(&mut Manifest) -> Result<(), String>) -> Result<Manifest, String> {
        let mut g = self.lock();
        // Another writer (the menu bar's provisioning) may have saved since.
        if let Ok(Some(disk)) = Manifest::load(&self.paths) {
            g.manifest = Some(disk);
        }
        let m = g.manifest.as_mut().ok_or("no shard is provisioned yet")?;
        f(m)?;
        m.save(&self.paths)?;
        Ok(m.clone())
    }

    fn set_state(&self, s: ShardState) {
        let mut g = self.lock();
        if g.state != s {
            info!("shard: {}", s.label());
        }
        g.state = s;
    }

    fn reload(&self) -> Result<(), String> {
        let m = Manifest::load(&self.paths)?;
        self.lock().manifest = m;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Main loop

    pub fn run(self) -> Result<(), String> {
        self.paths.ensure().map_err(|e| e.to_string())?;
        crate::logging::init(&self.paths.log_file("ign1t10n"));
        info!("supervisor {} starting (pid {})", env!("CARGO_PKG_VERSION"), std::process::id());
        unsafe {
            libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
        }
        let _ = std::fs::write(self.paths.pidfile(), std::process::id().to_string());
        self.reload()?;
        self.serve()?;
        if self.lock().manifest.as_ref().map(|m| m.stages.prepared()).unwrap_or(false) {
            self.spawn_job("start", |s| s.start());
        }
        let mut last_health = Instant::now() - Duration::from_secs(60);
        let mut last_audit = Instant::now();
        loop {
            if TERM.load(Ordering::SeqCst) || self.lock().shutdown {
                info!("stopping on request");
                self.stop_nodes();
                let _ = std::fs::remove_file(self.paths.socket());
                let _ = std::fs::remove_file(self.paths.pidfile());
                info!("supervisor exiting");
                return Ok(());
            }
            self.reap();
            if last_health.elapsed() >= Duration::from_secs(10) {
                last_health = Instant::now();
                self.health();
            }
            if last_audit.elapsed() >= Duration::from_secs(60) {
                last_audit = Instant::now();
                self.audit_loopback();
                self.breeder_tick();
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn spawn_job(&self, what: &'static str, f: impl FnOnce(&Supervisor) -> Result<(), String> + Send + 'static) -> bool {
        {
            let mut g = self.lock();
            if g.busy {
                return false;
            }
            g.busy = true;
        }
        let s = self.clone();
        std::thread::spawn(move || {
            let _awake = if what == "start" || what == "resize" { crate::platform::prevent_idle_sleep(what) } else { None };
            if let Err(e) = f(&s) {
                error!("{what}: {e}");
                if what == "start" {
                    // Do not leave half a shard running (or restarting).
                    s.stop_nodes();
                }
                s.set_state(ShardState::Failed(e.clone()));
                crate::platform::notify("Local shard", &format!("{what} failed: {e}"));
            }
            s.lock().busy = false;
        });
        true
    }

    // ------------------------------------------------------------------
    // Starting

    fn node_env(&self, role: &Role) -> Result<Vec<(String, String)>, String> {
        let mut env = procs::base_env();
        if !matches!(role, Role::Observer) {
            let pw = secrets::password(&*self.secrets, &role.name())?;
            env.push(("F1R3NODE_VALIDATOR_PASSWORD".into(), pw));
        }
        Ok(env)
    }

    fn api(&self, role: &Role) -> Result<Api, String> {
        let m = self.manifest()?;
        let base = match role {
            Role::Bootstrap => m.bootstrap.base_port,
            Role::Observer => m.observer.base_port,
            Role::Validator(k) => m.validator(*k).ok_or("no such validator")?.base_port,
            Role::Embers | Role::Portal => return Err(format!("{} is not a node", role.name())),
        };
        Ok(Api::new(Block(base).http_url()))
    }

    fn observer(&self) -> Result<Api, String> {
        self.api(&Role::Observer)
    }

    fn spawn_node(&self, role: Role) -> Result<(), String> {
        let m = self.manifest()?;
        let (program, args, env) = if role == Role::Embers {
            let e = crate::embers::load_or_create(&*self.secrets)?;
            let slot = m.embers.as_ref().and_then(|x| x.validator_slot).ok_or("embers has no validator")?;
            (self.paths.embers_bin.clone(), vec![], crate::embers::env(&m, &e, slot)?)
        } else if role == Role::Portal {
            let sec = crate::games::load_or_create(&*self.secrets)?;
            let args = vec!["-c".to_string(), self.paths.games_conf().display().to_string(), "serve".to_string()];
            (self.paths.games_bin.clone(), args, crate::games::env(&sec, crate::games::Grant { portal: true, ..Default::default() }))
        } else {
            (self.paths.node_bin.clone(), procs::node_args(&self.paths, &m, &role)?, self.node_env(&role)?)
        };
        let p = procs::spawn(&program, &args, &env, self.paths.stdout_log(&role.name()), role.clone())?;
        info!("started {} (pid {})", role.name(), p.child.id());
        let mut g = self.lock();
        g.restart_at.remove(&role);
        g.procs.insert(role, p);
        Ok(())
    }

    fn running(&self, role: &Role) -> bool {
        let mut g = self.lock();
        match g.procs.get_mut(role) {
            Some(p) => matches!(p.child.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn wait_ready(&self, role: &Role, timeout: Duration) -> Result<(), String> {
        if *role != Role::Portal && matches!(self.lock().state, ShardState::Starting(_) | ShardState::Stopped | ShardState::Failed(_)) {
            self.set_state(ShardState::Starting(format!("waiting for {}", role.name())));
        }
        let timeout = timeout.min(start_timeout_cap());
        let t0 = Instant::now();
        let check: Box<dyn Fn() -> Result<(), String>> = if *role == Role::Embers {
            let api = Api::new(format!("http://127.0.0.1:{}", self.manifest()?.embers.as_ref().map(|e| e.port).unwrap_or(0)));
            Box::new(move || api.get_ok("/api/service/ready"))
        } else if *role == Role::Portal {
            let g = self.manifest()?.games.ok_or("F1R3Games is not configured")?;
            let api = Api::new(g.url());
            let path = crate::games::ready_path(&g);
            Box::new(move || api.get_ok(&path))
        } else {
            let api = self.api(role)?;
            Box::new(move || api.ready_why())
        };
        let mut last_note = Instant::now();
        loop {
            let why = match check() {
                Ok(()) => {
                    info!("{} ready after {}s", role.name(), t0.elapsed().as_secs());
                    return Ok(());
                }
                Err(e) => e,
            };
            if !self.running(role) {
                return Err(format!("{} exited while starting; see {}", role.name(), self.paths.stdout_log(&role.name()).display()));
            }
            if t0.elapsed() > timeout {
                return Err(format!("{} not ready after {}s (last answer: {why})", role.name(), timeout.as_secs()));
            }
            if last_note.elapsed() >= Duration::from_secs(15) {
                last_note = Instant::now();
                info!("{} not ready yet after {}s: {why}", role.name(), t0.elapsed().as_secs());
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn check_ports(&self, m: &Manifest) -> Result<(), String> {
        let mut blocks: Vec<(String, Vec<u16>)> = vec![
            ("bootstrap".into(), Block(m.bootstrap.base_port).all().to_vec()),
            ("observer".into(), Block(m.observer.base_port).all().to_vec()),
        ];
        for v in m.validators.iter().filter(|v| v.state.runs()) {
            blocks.push((crate::nodeconf::validator_name(v.slot), Block(v.base_port).all().to_vec()));
        }
        if let Some(e) = m.embers.as_ref().filter(|_| m.options.embers) {
            blocks.push(("embers".into(), vec![e.port]));
        }
        for (name, ports) in blocks {
            if self.running(&Role::parse(&name).unwrap_or(Role::Bootstrap)) {
                continue;
            }
            for p in ports {
                if !ports::port_free(p) {
                    let who = ports::holder(p).unwrap_or_else(|| "another program".into());
                    return Err(format!("port {p} ({name}) in use by {who}; choose new ports"));
                }
            }
        }
        Ok(())
    }

    /// Start the shard (spec §7.1), performing genesis if it has not happened.
    pub fn start(&self) -> Result<(), String> {
        self.reload()?;
        let m = self.manifest()?;
        if !m.stages.prepared() {
            return Err("provisioning has not finished".into());
        }
        crate::lifecycle::check_compat(&m)?;
        self.lock().want = true;
        crate::nodeconf::write_all(&self.paths, &m)?;
        self.check_ports(&m)?;
        let genesis = m.shard.genesis_hash.is_none();

        // Bootstrap; learn its node id.
        self.set_state(ShardState::Starting("bootstrap".into()));
        if !self.running(&Role::Bootstrap) {
            self.spawn_node(Role::Bootstrap)?;
        }
        // During the genesis ceremony the bootstrap has no Casper engine yet, so
        // /api/status cannot answer; but the node prints its identity at
        // startup ("Local peer node: rnode://<id>@..."), which reaches
        // bootstrap.stdout.log. Read whichever comes first.
        let boot = self.api(&Role::Bootstrap)?;
        let log = self.paths.stdout_log("bootstrap");
        let started = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        let id = admin::wait_for("the bootstrap's node id", Duration::from_secs(120), Duration::from_secs(1), || {
            if !self.running(&Role::Bootstrap) {
                return Some(Err(format!("the bootstrap exited; see {}", log.display())));
            }
            node_id_from_log(&log, started).or_else(|| boot.status().ok().and_then(|s| s.node_id)).map(Ok)
        })??;
        if m.bootstrap.node_id.as_deref() != Some(&id) {
            self.update(|m| {
                m.bootstrap.node_id = Some(id.clone());
                Ok(())
            })?;
        }

        // Validators, in parallel.
        let slots: Vec<u8> = self.manifest()?.validators.iter().filter(|v| v.state.runs()).map(|v| v.slot).collect();
        self.set_state(ShardState::Starting(if genesis { "genesis ceremony".into() } else { "validators".into() }));
        for &k in &slots {
            if !self.running(&Role::Validator(k)) {
                self.spawn_node(Role::Validator(k))?;
            }
        }
        let t = Duration::from_secs(if genesis { 600 } else { 180 });
        let waits: Vec<_> = slots
            .iter()
            .map(|&k| {
                let s = self.clone();
                std::thread::spawn(move || s.wait_ready(&Role::Validator(k), t))
            })
            .collect();
        for w in waits {
            w.join().map_err(|_| "a wait panicked")??;
        }
        if genesis {
            self.wait_ready(&Role::Bootstrap, Duration::from_secs(120))?;
            let v1 = self.api(&Role::Validator(slots[0]))?;
            let hash = admin::wait_for("the genesis block", Duration::from_secs(60), Duration::from_secs(1), || v1.genesis_hash().ok())?;
            let st = v1.status().ok();
            self.update(|m| {
                m.shard.genesis_hash = Some(hash.clone());
                m.shard.genesis_node_version = st.as_ref().map(|s| s.node_version.clone());
                m.shard.storage_format = st.as_ref().and_then(|s| s.storage_format);
                m.stages.genesis = true;
                Ok(())
            })?;
            info!("genesis {hash}");
        }

        // Observer.
        self.set_state(ShardState::Starting("observer".into()));
        if !self.running(&Role::Observer) {
            self.spawn_node(Role::Observer)?;
        }
        self.wait_ready(&Role::Observer, Duration::from_secs(if genesis { 300 } else { 180 }))?;
        if genesis {
            let obs = self.observer()?;
            for v in self.manifest()?.validators.iter().filter(|v| v.genesis) {
                if !obs.bond_status(&v.public_key).unwrap_or(false) {
                    warn!("genesis validator {} not reported bonded", v.slot);
                }
            }
        }

        // Embers.
        if self.manifest()?.options.embers {
            self.start_embers()?;
        }

        // Where F1R3Gaze deploys: redrawn at each start (Decision 5).
        let mut m = self.manifest()?;
        if m.stages.gaze {
            crate::gaze::refresh(&self.paths, &mut m)?;
            self.update(|x| {
                x.wallet.gaze_validators = m.wallet.gaze_validators.clone();
                Ok(())
            })?;
        }
        self.set_state(ShardState::Running);
        self.audit_loopback();

        // F1R3Games: its stages and the portal. A failure here leaves the
        // shard running and F1R3Games Failed (spec v0.4 §7).
        self.start_games();

        // Resume an interrupted resize.
        if let Some(r) = self.manifest()?.resize.filter(|r| r.failed.is_none()) {
            info!("resuming resize to {}", r.target);
            self.run_resize(r)?;
        }
        Ok(())
    }

    fn random_active(&self, except: Option<u8>) -> Result<(u8, Api), String> {
        let m = self.manifest()?;
        let mut c: Vec<&Validator> = m.active().into_iter().filter(|v| Some(v.slot) != except).collect();
        c.shuffle(&mut rand::thread_rng());
        let v = c.first().ok_or("no active validator to send a deploy to")?;
        Ok((v.slot, Api::new(Block(v.base_port).http_url())))
    }

    fn start_embers(&self) -> Result<(), String> {
        let m = self.manifest()?;
        let e = m.embers.clone().ok_or("embers is enabled but not configured")?;
        if !e.funded {
            self.set_state(ShardState::Starting("funding Embers".into()));
            let a = self.faucet_transfer(&e.service.address, dust(EMBERS_SERVICE_F1R3))?;
            info!("funded embers service ({a})");
            self.faucet_transfer(&e.testnet_service.address, dust(EMBERS_SERVICE_F1R3))?;
            self.update(|m| {
                m.embers.as_mut().unwrap().funded = true;
                Ok(())
            })?;
        }
        let (slot, _) = self.random_active(None)?;
        self.update(|m| {
            m.embers.as_mut().unwrap().validator_slot = Some(slot);
            Ok(())
        })?;
        if !self.running(&Role::Embers) {
            self.spawn_node(Role::Embers)?;
        }
        self.wait_ready(&Role::Embers, Duration::from_secs(300))
    }

    // ------------------------------------------------------------------
    // Stopping

    fn stop_role(&self, role: &Role, grace: Duration) {
        let p = {
            let mut g = self.lock();
            g.restart_at.remove(role);
            g.exits.remove(role);
            g.procs.remove(role)
        };
        if let Some(mut p) = p {
            let clean = procs::stop_all(std::slice::from_mut(&mut p), grace);
            info!("stopped {}{}", role.name(), if clean { "" } else { " (killed)" });
        }
    }

    /// Ordered stop (spec §7.4): observer and Embers, then every validator at
    /// once, then the bootstrap.
    pub fn stop_nodes(&self) {
        {
            // No restart that was already scheduled may outlive the stop.
            let mut g = self.lock();
            g.want = false;
            g.restart_at.clear();
            g.exits.clear();
        }
        self.stop_role(&Role::Portal, Duration::from_secs(30));
        self.stop_role(&Role::Embers, Duration::from_secs(30));
        self.stop_role(&Role::Observer, Duration::from_secs(30));
        let mut vs: Vec<Proc> = {
            let mut g = self.lock();
            let keys: Vec<Role> = g.procs.keys().filter(|r| matches!(r, Role::Validator(_))).cloned().collect();
            keys.into_iter().filter_map(|k| g.procs.remove(&k)).collect()
        };
        if !vs.is_empty() {
            procs::stop_all(&mut vs, Duration::from_secs(60));
            info!("stopped {} validators", vs.len());
        }
        self.stop_role(&Role::Bootstrap, Duration::from_secs(30));
        self.set_state(ShardState::Stopped);
        self.games_waiting();
    }

    // ------------------------------------------------------------------
    // Restarts and health

    fn reap(&self) {
        let mut restart: Vec<Role> = vec![];
        let mut failed: Option<String> = None;
        {
            let mut g = self.lock();
            let exited: Vec<(Role, String)> = g
                .procs
                .iter_mut()
                .filter_map(|(r, p)| match p.child.try_wait() {
                    Ok(Some(st)) => Some((r.clone(), format!("{st}"))),
                    _ => None,
                })
                .collect();
            for (r, st) in exited {
                g.procs.remove(&r);
                if !g.want {
                    continue;
                }
                warn!("{} exited unexpectedly ({st})", r.name());
                let now = Instant::now();
                let e = g.exits.entry(r.clone()).or_default();
                e.push(now);
                e.retain(|t| now.duration_since(*t) < FAIL_WINDOW);
                if e.len() >= FAILS_ALLOWED {
                    let why = format!("{} exited {} times in five minutes", r.name(), e.len());
                    if matches!(r, Role::Portal | Role::Embers) {
                        // A service failing does not stop the shard (spec v0.4 §8.3).
                        g.restart_at.remove(&r);
                        if r == Role::Portal {
                            g.games = GamesState::Failed(why.clone());
                        }
                        error!("{why}; leaving it stopped");
                        crate::platform::notify(if r == Role::Portal { "F1R3Games stopped" } else { "Embers stopped" }, &format!("{why}. See the logs in {}.", self.paths.logs.display()));
                        continue;
                    }
                    failed = Some(why);
                    break;
                }
                let n = g.restart_at.get(&r).map(|x| x.1 + 1).unwrap_or(0);
                let delay = Duration::from_secs((1u64 << n.min(6)).min(60));
                g.restart_at.insert(r.clone(), (now + delay, n));
            }
            if failed.is_none() && g.want && !matches!(g.state, ShardState::Stopped | ShardState::Failed(_)) {
                let now = Instant::now();
                restart = g.restart_at.iter().filter(|(r, (t, _))| *t <= now && !g.procs.contains_key(*r)).map(|(r, _)| r.clone()).collect();
            }
        }
        if let Some(reason) = failed {
            // Never leave a bonded validator down while the others run.
            error!("{reason}; stopping the shard");
            self.stop_nodes();
            self.set_state(ShardState::Failed(reason.clone()));
            crate::platform::notify("Local shard stopped", &format!("{reason}. See the logs in {}.", self.paths.logs.display()));
            return;
        }
        for r in restart {
            match self.spawn_node(r.clone()) {
                Ok(()) => {
                    // Keep the back-off count; clear the timer.
                    let mut g = self.lock();
                    let n = g.restart_at.get(&r).map(|x| x.1).unwrap_or(0);
                    g.restart_at.insert(r, (Instant::now() + Duration::from_secs(3600), n));
                }
                Err(e) => error!("restart {}: {e}", r.name()),
            }
        }
    }

    fn health(&self) {
        self.games_health();
        let Ok(m) = self.manifest() else { return };
        let mut roles = vec![Role::Bootstrap, Role::Observer];
        roles.extend(m.validators.iter().filter(|v| v.state.runs()).map(|v| Role::Validator(v.slot)));
        let mut h = BTreeMap::new();
        for r in &roles {
            let st = self.api(r).ok().and_then(|a| a.status().ok());
            h.insert(r.clone(), st.map(|s| (s.is_ready, s.lfb, s.peers)).unwrap_or((false, -1, 0)));
        }
        let mut g = self.lock();
        g.health = h.clone();
        if !matches!(g.state, ShardState::Running | ShardState::Degraded(_)) {
            return;
        }
        let mut reasons = vec![];
        for (r, (ready, _, _)) in &h {
            if !ready {
                reasons.push(format!("{} not ready", r.name()));
            }
        }
        let vl: Vec<i64> = h.iter().filter(|(r, _)| matches!(r, Role::Validator(_))).map(|(_, x)| x.1).filter(|x| *x >= 0).collect();
        if let (Some(lo), Some(hi)) = (vl.iter().min(), vl.iter().max()) {
            if hi - lo > 20 {
                reasons.push(format!("validators differ by {} blocks", hi - lo));
            }
            let obs = h.get(&Role::Observer).map(|x| x.1).unwrap_or(-1);
            if hi - obs > 20 {
                let since = *g.lag_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(60) {
                    reasons.push(format!("observer {} blocks behind", hi - obs));
                }
            } else {
                g.lag_since = None;
            }
        }
        if h.get(&Role::Observer).map(|x| x.2 == 0).unwrap_or(false) {
            reasons.push("observer has no peers".into());
        }
        if !g.exposed.is_empty() {
            reasons.push("listening off loopback".into());
        }
        let s = if reasons.is_empty() { ShardState::Running } else { ShardState::Degraded(reasons.join("; ")) };
        if g.state != s {
            info!("shard: {}", s.label());
        }
        g.state = s;
    }

    /// A3: no node socket may listen off loopback. Until N1 lands the
    /// transport binds 0.0.0.0; this makes that visible instead of silent.
    fn audit_loopback(&self) {
        let pids: Vec<String> = self.lock().procs.values().map(|p| p.child.id().to_string()).collect();
        if pids.is_empty() {
            return;
        }
        let Ok(out) = std::process::Command::new("lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pids.join(","), "-Fn"]).output() else { return };
        let exposed: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix('n'))
            .filter(|a| !(a.starts_with("127.0.0.1:") || a.starts_with("[::1]:") || a.starts_with("localhost:")))
            .map(str::to_string)
            .collect();
        if !exposed.is_empty() {
            warn!("sockets listening off loopback: {}", exposed.join(", "));
        }
        self.lock().exposed = exposed;
    }

    // ------------------------------------------------------------------
    // Deploys

    fn key(&self, name: &str) -> Result<Key, String> {
        let pw = secrets::password(&*self.secrets, name)?;
        Key::read(&self.paths.key(name), &pw)
    }

    /// Faucet → address; waits until finalised. Returns the deploy id.
    pub fn faucet_transfer(&self, to: &str, amount: i64) -> Result<String, String> {
        let m = self.manifest()?;
        let faucet = self.key("faucet")?;
        let from = Address::parse(&m.faucet.address)?;
        let to = Address::parse(to)?;
        let (_, target) = self.random_active(None)?;
        let sent = admin::submit(&target, &faucet, &crate::rho::transfer(&from, &to, amount), &m.shard.id)?;
        admin::await_finalized(&target, &sent.id, Duration::from_secs(300))?;
        Ok(sent.id)
    }

    // ------------------------------------------------------------------
    // Resizing

    fn run_resize(&self, r: crate::manifest::Resize) -> Result<(), String> {
        self.update(|m| {
            m.resize = Some(r.clone());
            Ok(())
        })?;
        self.set_state(ShardState::Resizing(format!("to {} validators", r.target)));
        let mut ops = SupOps { sup: self.clone() };
        match resize::run(&mut ops, &r) {
            Ok(()) => {
                self.update(|m| {
                    m.resize = None;
                    Ok(())
                })?;
                crate::nodeconf::write_all(&self.paths, &self.manifest()?)?;
                let mut m = self.manifest()?;
                if m.stages.gaze {
                    crate::gaze::refresh(&self.paths, &mut m)?;
                    self.update(|x| {
                        x.wallet.gaze_validators = m.wallet.gaze_validators.clone();
                        Ok(())
                    })?;
                }
                self.lock().resize_msg = None;
                self.set_state(ShardState::Running);
                info!("resize complete: {} validators", self.manifest()?.n());
                Ok(())
            }
            Err(e) => {
                self.update(|m| {
                    if let Some(x) = m.resize.as_mut() {
                        x.failed = Some(e.clone());
                    }
                    Ok(())
                })?;
                // The shard keeps running with the slots completed so far.
                self.set_state(ShardState::Degraded(format!("resize stopped: {e} (Retry or Undo)")));
                crate::platform::notify("Resize stopped", &e);
                Ok(())
            }
        }
    }

    fn new_shard(&self, n0: u8) -> Result<(), String> {
        self.set_state(ShardState::Resizing(format!("new shard with {n0} validators")));
        self.stop_nodes();
        let m = self.manifest()?;
        crate::lifecycle::archive_shard(&self.paths, &*self.secrets, &m)?;
        crate::provision::reprovision(&self.paths, &*self.secrets, &m, n0)?;
        self.start()
    }

    // ------------------------------------------------------------------
    // The control socket

    fn serve(&self) -> Result<(), String> {
        let sock = self.paths.socket();
        if UnixStream::connect(&sock).is_ok() {
            return Err("another supervisor is already running".into());
        }
        let _ = std::fs::remove_file(&sock);
        let l = UnixListener::bind(&sock).map_err(|e| format!("{}: {e}", sock.display()))?;
        crate::paths::set_mode(&sock, 0o600).ok();
        let s = self.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                if control::peer_uid(&c) != Some(control::my_uid()) {
                    warn!("refused a control connection from another user");
                    continue;
                }
                let s = s.clone();
                std::thread::spawn(move || s.connection(c));
            }
        });
        Ok(())
    }

    fn connection(&self, c: UnixStream) {
        let mut w = match c.try_clone() {
            Ok(w) => w,
            Err(_) => return,
        };
        for line in BufReader::new(c).lines().map_while(Result::ok) {
            let resp = match serde_json::from_str::<Request>(&line) {
                Ok(r) => self.handle(r),
                Err(e) => Response::err(format!("bad request: {e}")),
            };
            let mut out = serde_json::to_string(&resp).unwrap();
            out.push('\n');
            if w.write_all(out.as_bytes()).is_err() {
                return;
            }
        }
    }

    pub fn handle(&self, r: Request) -> Response {
        match r {
            Request::Status => Response { ok: true, status: Some(self.report()), ..Default::default() },
            Request::Start => {
                if let Err(e) = self.reload() {
                    return Response::err(e);
                }
                self.lock().exits.clear();
                if self.spawn_job("start", |s| s.start()) { Response::ok("starting") } else { Response::err("busy; try again shortly") }
            }
            Request::Stop => {
                if self.lock().busy {
                    return Response::err("an operation is in progress; stop it after it finishes, or quit to stop at once");
                }
                self.stop_nodes();
                Response::ok("stopped")
            }
            Request::Restart => {
                let ok = self.spawn_job("start", |s| {
                    s.stop_nodes();
                    s.start()
                });
                if ok { Response::ok("restarting") } else { Response::err("busy; try again shortly") }
            }
            Request::Resize { target, new_shard } => {
                let m = match self.manifest() {
                    Ok(m) => m,
                    Err(e) => return Response::err(e),
                };
                let plan = match resize::plan(&m, target, new_shard) {
                    Ok(p) => p,
                    Err(e) => return Response::err(e),
                };
                let ok = if new_shard { self.spawn_job("resize", move |s| s.new_shard(target)) } else { self.spawn_job("resize", move |s| s.run_resize(plan)) };
                if ok { Response::ok(format!("resizing to {target} validators")) } else { Response::err("busy; try again shortly") }
            }
            Request::Retry | Request::Undo => {
                let undo = r == Request::Undo;
                let Some(mut plan) = self.manifest().ok().and_then(|m| m.resize) else { return Response::err("no resize to retry or undo") };
                if undo {
                    plan = resize::undo_plan(&SupOps { sup: self.clone() }, &plan);
                    let _ = self.update(|m| {
                        m.resize = None;
                        Ok(())
                    });
                } else {
                    plan.failed = None;
                    let _ = self.update(|m| {
                        m.resize = None;
                        Ok(())
                    });
                }
                if self.spawn_job("resize", move |s| s.run_resize(plan)) { Response::ok(if undo { "undoing" } else { "retrying" }) } else { Response::err("busy") }
            }
            Request::Fund { address, f1r3 } => {
                if !(1..=1_000_000).contains(&f1r3) {
                    return Response::err("fund between 1 and 1,000,000 F1R3 at a time");
                }
                if let Err(e) = Address::parse(&address) {
                    return Response::err(e);
                }
                match self.faucet_transfer(&address, dust(f1r3)) {
                    Ok(id) => Response::ok(format!("sent {} to {address} (deploy {}…)", show(dust(f1r3)), &id[..12])),
                    Err(e) => Response::err(e),
                }
            }
            Request::SetOptions { embers, gaze_integration } => match self.set_options(embers, gaze_integration) {
                Ok(()) => Response::ok("options updated"),
                Err(e) => Response::err(e),
            },
            Request::Reallocate { node } => match self.reallocate(&node) {
                Ok(msg) => Response::ok(msg),
                Err(e) => Response::err(e),
            },
            Request::GamesSet { on, breeder, faucet_f1r3, open_at_first_run } => match self.games_set(on, breeder, faucet_f1r3, open_at_first_run) {
                Ok(msg) => Response::ok(msg),
                Err(e) => Response::err(e),
            },
            Request::GamesReinstall => match self.games_job("reinstall", |s| s.games_reinstall()) {
                true => Response::ok("reinstalling F1R3Games"),
                false => Response::err("busy; try again shortly"),
            },
            Request::GamesUpdate => match self.games_job("update", |s| s.games_update()) {
                true => Response::ok("updating F1R3Games"),
                false => Response::err("busy; try again shortly"),
            },
            Request::GamesMove => match self.games_job("move", |s| s.games_move()) {
                true => Response::ok("moving F1R3Games to new ports"),
                false => Response::err("busy; try again shortly"),
            },
            Request::Shutdown => {
                self.lock().shutdown = true;
                Response::ok("shutting down")
            }
        }
    }

    fn set_options(&self, embers: Option<bool>, gaze_integration: Option<bool>) -> Result<(), String> {
        if let Some(g) = gaze_integration {
            let mut m = self.update(|m| {
                m.options.gaze_integration = g;
                Ok(())
            })?;
            if g {
                crate::gaze::refresh(&self.paths, &mut m)?;
                m.stages.gaze = true;
                let v = m.wallet.gaze_validators.clone();
                self.update(|x| {
                    x.wallet.gaze_validators = v;
                    x.stages.gaze = true;
                    Ok(())
                })?;
            } else {
                crate::gaze::remove(&self.paths.profile)?;
            }
        }
        if let Some(e) = embers {
            if e {
                crate::provision::configure_embers(&self.paths, &*self.secrets, &mut self.manifest()?).and_then(|m| {
                    self.update(|x| {
                        x.embers = m.embers.clone();
                        x.options.embers = true;
                        Ok(())
                    })
                })?;
                if matches!(self.lock().state, ShardState::Running | ShardState::Degraded(_)) && !self.spawn_job("embers", |s| s.start_embers().map(|_| s.set_state(ShardState::Running))) {
                    return Err("busy; Embers will start with the shard".into());
                }
            } else {
                self.stop_role(&Role::Embers, Duration::from_secs(30));
                self.update(|x| {
                    x.options.embers = false;
                    Ok(())
                })?;
            }
            let mut m = self.manifest()?;
            if m.stages.gaze && m.options.gaze_integration {
                crate::gaze::refresh(&self.paths, &mut m)?;
            }
        }
        Ok(())
    }

    fn reallocate(&self, node: &str) -> Result<String, String> {
        if self.lock().busy {
            return Err("busy".into());
        }
        let role = Role::parse(node).ok_or_else(|| format!("no node {node}"))?;
        if self.running(&role) {
            return Err(format!("{node} is running; stop the shard first"));
        }
        let m = self.update(|m| {
            let mut taken = m.taken_ports();
            let own = match &role {
                Role::Bootstrap => m.bootstrap.base_port,
                Role::Observer => m.observer.base_port,
                Role::Validator(k) => m.validator(*k).ok_or("no such validator")?.base_port,
                Role::Embers => m.embers.as_ref().ok_or("no embers")?.port,
            Role::Portal => return Err("F1R3Games' ports are its browser origins; move it with \"Move F1R3Games to a new address\" (ctl games move --yes)".into()),
            };
            taken.retain(|p| !(own..own + 6).contains(p));
            match &role {
                Role::Embers => m.embers.as_mut().unwrap().port = ports::allocate_single(own + 1, &taken).ok_or("no free port")?,
                _ => {
                    let b = ports::allocate(own + 10, &taken).ok_or("no free ports")?.0;
                    match &role {
                        Role::Bootstrap => m.bootstrap.base_port = b,
                        Role::Observer => m.observer.base_port = b,
                        Role::Validator(k) => m.validator_mut(*k).unwrap().base_port = b,
                        _ => {}
                    }
                }
            }
            Ok(())
        })?;
        crate::nodeconf::write_all(&self.paths, &m)?;
        if m.stages.gaze && m.options.gaze_integration {
            crate::gaze::refresh(&self.paths, &mut m.clone())?;
        }
        Ok(format!("{node} moved; start the shard"))
    }

    pub fn report(&self) -> Report {
        let g = self.lock();
        let Some(m) = g.manifest.as_ref() else {
            return Report { shard: g.state.clone(), ..Default::default() };
        };
        let node = |name: String, role: Role, base: u16, state: String, pk: Option<String>| {
            let (ready, lfb, peers) = g.health.get(&role).copied().unwrap_or((false, -1, 0));
            NodeReport { name, running: g.procs.contains_key(&role), base_port: base, state, ready, lfb, peers, public_key: pk }
        };
        let mut nodes = vec![node("bootstrap".into(), Role::Bootstrap, m.bootstrap.base_port, "bootstrap".into(), None)];
        for v in m.validators.iter().filter(|v| v.state != SlotState::Absent) {
            nodes.push(node(crate::nodeconf::validator_name(v.slot), Role::Validator(v.slot), v.base_port, v.state.name().into(), Some(v.public_key.clone())));
        }
        nodes.push(node("observer".into(), Role::Observer, m.observer.base_port, "observer".into(), None));
        if let Some(e) = m.embers.as_ref().filter(|_| m.options.embers) {
            let mut n = node("embers".into(), Role::Embers, e.port, "embers".into(), None);
            n.ready = g.procs.contains_key(&Role::Embers);
            nodes.push(n);
        }
        if let Some(x) = m.games.as_ref().filter(|_| m.options.games) {
            let mut n = node("portal".into(), Role::Portal, x.portal_port, "portal".into(), None);
            n.ready = g.procs.contains_key(&Role::Portal) && g.games == GamesState::Running;
            nodes.push(n);
        }
        Report {
            shard: g.state.clone(),
            validators: m.n(),
            nodes,
            shard_id: m.shard.id.clone(),
            genesis_hash: m.shard.genesis_hash.clone(),
            gaze_validators: m.wallet.gaze_validators.clone(),
            observer: Block(m.observer.base_port).http_url(),
            embers: m.embers.as_ref().filter(|_| m.options.embers).map(|e| format!("http://127.0.0.1:{}", e.port)),
            funded_wallet: m.wallet.funded.clone(),
            resize: m.resize.as_ref().map(|r| {
                let base = format!("{} to {} validators", if r.undo { "undoing" } else if r.grow { "growing" } else { "shrinking" }, r.target);
                match (&r.failed, &g.resize_msg) {
                    (Some(f), _) => format!("{base}: stopped: {f}"),
                    (None, Some(p)) => format!("{base}: {p}"),
                    _ => base,
                }
            }),
            exposed: g.exposed.clone(),
            embers_enabled: m.options.embers,
            gaze_integration: m.options.gaze_integration,
            games: games_sup::report(m, &g.games),
        }
    }
}

/// The node id a node printed at startup ("Local peer node: rnode://<id>@…"),
/// looking only at what was written after byte `from` of its output log.
pub fn node_id_from_log(log: &std::path::Path, from: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(log).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(if from <= len { from } else { 0 })).ok()?;
    let mut text = String::new();
    f.take(4 << 20).read_to_string(&mut text).ok()?;
    text.lines().rev().find_map(|l| {
        let i = l.find("rnode://")?;
        crate::api::node_id_of(l[i..].split(['"', ' ']).next()?)
    })
}

/// The resize engine's effects, performed by the supervisor.
struct SupOps {
    sup: Supervisor,
}

impl SupOps {
    fn v(&self, slot: u8) -> Result<Validator, String> {
        self.sup.manifest()?.validator(slot).cloned().ok_or_else(|| format!("no validator {slot}"))
    }
    fn obs(&self) -> Result<Api, String> {
        self.sup.observer()
    }
    /// Move F1R3Gaze and Embers off `slot` before it withdraws.
    fn evacuate(&self, slot: u8) -> Result<(), String> {
        let mut m = self.sup.manifest()?;
        let url = Block(self.v(slot)?.base_port).http_url();
        if let Some(v) = m.validator_mut(slot) {
            v.state = SlotState::Retiring; // excluded from the draw
        }
        if m.stages.gaze && m.options.gaze_integration && m.wallet.gaze_validators.contains(&url) {
            crate::gaze::refresh(&self.sup.paths, &mut m)?;
            let v = m.wallet.gaze_validators.clone();
            self.sup.update(|x| {
                x.wallet.gaze_validators = v;
                Ok(())
            })?;
            info!("F1R3Gaze moved off validator {slot}");
        }
        if m.options.embers && m.embers.as_ref().and_then(|e| e.validator_slot) == Some(slot) {
            self.sup.stop_role(&Role::Embers, Duration::from_secs(30));
            let (other, _) = self.sup.random_active(Some(slot))?;
            self.sup.update(|x| {
                x.embers.as_mut().unwrap().validator_slot = Some(other);
                Ok(())
            })?;
            self.sup.spawn_node(Role::Embers)?;
            info!("Embers moved to validator {other}");
        }
        if m.options.games && self.sup.running(&Role::Portal) {
            self.sup.games_retarget(Some(slot))?;
            info!("F1R3Games moved off validator {slot}");
        }
        Ok(())
    }
}

impl Ops for SupOps {
    fn state(&self, slot: u8) -> SlotState {
        self.sup.manifest().ok().and_then(|m| m.validator(slot).map(|v| v.state)).unwrap_or(SlotState::Absent)
    }
    fn set_state(&mut self, slot: u8, s: SlotState) -> Result<(), String> {
        self.sup.update(|m| {
            m.validator_mut(slot).ok_or("no such validator")?.state = s;
            Ok(())
        })?;
        info!("validator {slot}: {}", s.name());
        Ok(())
    }
    fn progress(&mut self, slot: u8, msg: &str) {
        let t = format!("validator {slot}: {msg}");
        info!("{t}");
        self.sup.lock().resize_msg = Some(t.clone());
        self.sup.set_state(ShardState::Resizing(t));
    }
    fn prepare(&mut self, slot: u8) -> Result<(), String> {
        let name = crate::nodeconf::validator_name(slot);
        let key = Key::generate();
        let pw = crate::keys::new_password();
        self.sup.secrets.set(&secrets::pem_account(&name), &pw)?;
        key.write(&self.sup.paths.key(&name), &pw)?;
        let m = self.sup.update(|m| {
            let b = ports::allocate(ports::validator_base(slot), &m.taken_ports()).ok_or("no free ports")?.0;
            let v = Validator { slot, public_key: key.public_hex(), address: key.address(), base_port: b, state: SlotState::Keyed, genesis: false, deploy: None, balance_before_payout: None };
            match m.validator_mut(slot) {
                Some(x) => *x = v,
                None => {
                    m.validators.push(v);
                    m.validators.sort_by_key(|v| v.slot);
                }
            }
            Ok(())
        })?;
        crate::nodeconf::write_all(&self.sup.paths, &m)
    }
    fn fund(&mut self, slot: u8) -> Result<(), String> {
        let v = self.v(slot)?;
        let amount = dust(VALIDATOR_FEES_F1R3) + STAKE;
        let obs = self.obs()?;
        if obs.balance(&v.address).unwrap_or(0) < amount {
            self.sup.faucet_transfer(&v.address, amount)?;
        }
        admin::wait_for("the funds to arrive", Duration::from_secs(120), Duration::from_secs(2), || (obs.balance(&v.address).unwrap_or(0) >= amount).then_some(()))
    }
    fn start_and_wait(&mut self, slot: u8) -> Result<(), String> {
        let r = Role::Validator(slot);
        // The node must run while in `joining`; the engine records that after.
        self.sup.update(|m| {
            m.validator_mut(slot).unwrap().state = SlotState::Joining;
            Ok(())
        })?;
        if !self.sup.running(&r) {
            self.sup.spawn_node(r.clone())?;
        }
        let res = self.sup.wait_ready(&r, Duration::from_secs(600));
        if res.is_err() {
            // Not bonded: stop it rather than let restarts fail the shard.
            self.sup.stop_role(&r, Duration::from_secs(30));
            self.sup.update(|m| {
                m.validator_mut(slot).unwrap().state = SlotState::Funded;
                Ok(())
            })?;
        }
        res
    }
    fn bond(&mut self, slot: u8) -> Result<(), String> {
        let v = self.v(slot)?;
        let obs = self.obs()?;
        if obs.bond_status(&v.public_key).unwrap_or(false) {
            return Ok(());
        }
        let key = self.sup.key(&crate::nodeconf::validator_name(slot))?;
        let (_, target) = self.sup.random_active(Some(slot))?;
        let sent = admin::submit(&target, &key, &crate::rho::bond(STAKE), &self.sup.manifest()?.shard.id)?;
        self.sup.update(|m| {
            m.validator_mut(slot).unwrap().deploy = Some(sent.id.clone());
            Ok(())
        })?;
        admin::await_finalized(&target, &sent.id, Duration::from_secs(300))
    }
    fn await_bonded(&mut self, slot: u8) -> Result<(), String> {
        let v = self.v(slot)?;
        let obs = self.obs()?;
        admin::wait_for("the bond to take effect", Duration::from_secs(900), Duration::from_secs(3), || obs.bond_status(&v.public_key).ok().filter(|b| *b).map(|_| ()))
    }
    fn withdraw(&mut self, slot: u8) -> Result<(), String> {
        self.evacuate(slot)?;
        let v = self.v(slot)?;
        let obs = self.obs()?;
        let before = obs.balance(&v.address)?;
        let key = self.sup.key(&crate::nodeconf::validator_name(slot))?;
        let (_, target) = self.sup.random_active(Some(slot))?;
        let sent = admin::submit(&target, &key, &crate::rho::withdraw(), &self.sup.manifest()?.shard.id)?;
        self.sup.update(|m| {
            let x = m.validator_mut(slot).unwrap();
            x.deploy = Some(sent.id.clone());
            x.balance_before_payout = Some(before);
            Ok(())
        })?;
        admin::await_finalized(&target, &sent.id, Duration::from_secs(300))
    }
    fn await_left(&mut self, slot: u8) -> Result<(), String> {
        let v = self.v(slot)?;
        let obs = self.obs()?;
        let term = crate::rho::active_validators();
        admin::wait_for("the validator to leave the active set", Duration::from_secs(900), Duration::from_secs(3), || {
            obs.explore(&term).ok().filter(|a| !crate::rho::mentions_key(a, &v.public_key)).map(|_| ())
        })
    }
    fn stop(&mut self, slot: u8) -> Result<(), String> {
        self.sup.stop_role(&Role::Validator(slot), Duration::from_secs(60));
        Ok(())
    }
    fn await_payout(&mut self, slot: u8) -> Result<(), String> {
        let v = self.v(slot)?;
        let obs = self.obs()?;
        let before = v.balance_before_payout.unwrap_or(0);
        admin::wait_for("the stake to be paid out", Duration::from_secs(1200), Duration::from_secs(5), || obs.balance(&v.address).ok().filter(|b| *b >= before + STAKE).map(|_| ()))
    }
    fn sweep(&mut self, slot: u8) -> Result<(), String> {
        let v = self.v(slot)?;
        let bal = self.obs()?.balance(&v.address)?;
        let keep = dust(1); // covers the sweep's own phlo
        if bal <= keep {
            return Ok(());
        }
        let m = self.sup.manifest()?;
        let key = self.sup.key(&crate::nodeconf::validator_name(slot))?;
        let (_, target) = self.sup.random_active(Some(slot))?;
        let term = crate::rho::transfer(&Address::parse(&v.address)?, &Address::parse(&m.faucet.address)?, bal - keep);
        let sent = admin::submit(&target, &key, &term, &m.shard.id)?;
        admin::await_finalized(&target, &sent.id, Duration::from_secs(300))
    }
    fn archive(&mut self, slot: u8) -> Result<(), String> {
        let name = crate::nodeconf::validator_name(slot);
        crate::lifecycle::archive_slot(&self.sup.paths, &*self.sup.secrets, &name)?;
        self.sup.update(|m| {
            let x = m.validator_mut(slot).unwrap();
            x.deploy = None;
            x.balance_before_payout = None;
            Ok(())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn node_id_from_startup_log() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("bootstrap.stdout.log");
        std::fs::write(&log, "old line rnode://aaaa@127.0.0.1?protocol=1\n").unwrap();
        let from = std::fs::metadata(&log).unwrap().len();
        assert_eq!(super::node_id_from_log(&log, from), None, "only this run's output counts");
        let line = r#"{"message":"Local peer node: rnode://0d529f30dce004fe1ea5c88819eb3a36f0495e67@127.0.0.1?protocol=40400&discovery=40404","target":"node"}"#;
        std::fs::write(&log, format!("old line rnode://aaaa@127.0.0.1?protocol=1\n{line}\n")).unwrap();
        assert_eq!(super::node_id_from_log(&log, from).as_deref(), Some("0d529f30dce004fe1ea5c88819eb3a36f0495e67"));
    }
}
