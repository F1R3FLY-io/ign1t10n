//! First-run provisioning (spec §6): stages S0..S9, each recorded in
//! `shard.toml` when done, so an interrupted run resumes at the first
//! incomplete stage. S0..S6 and S8..S9 run in the menu-bar process (or in
//! `ctl provision` headless); S7, the genesis, is the supervisor's first start.

use crate::control::{self, Request, ShardState};
use crate::keys::Key;
use crate::manifest::*;
use crate::paths::Paths;
use crate::ports;
use crate::secrets::{self, Secrets};
use crate::{info, nodeconf};
use std::time::{Duration, Instant};

/// What the person chose on the installation configuration panel.
#[derive(Clone, Debug)]
pub struct Choices {
    pub validators: u8,
    pub embers: bool,
    pub gaze_integration: bool,
}

impl Default for Choices {
    fn default() -> Self {
        Choices { validators: crate::DEFAULT_VALIDATORS, embers: true, gaze_integration: true }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    Preflight,
    Ports,
    Wallet,
    Keys,
    GenesisFiles,
    Configs,
    Agent,
    Genesis,
    Gaze,
    Opened,
}

impl Stage {
    pub const ALL: [Stage; 10] = [Stage::Preflight, Stage::Ports, Stage::Wallet, Stage::Keys, Stage::GenesisFiles, Stage::Configs, Stage::Agent, Stage::Genesis, Stage::Gaze, Stage::Opened];
    pub fn title(self) -> &'static str {
        match self {
            Stage::Preflight => "Checking this Mac",
            Stage::Ports => "Choosing ports",
            Stage::Wallet => "Finding your F1R3Gaze wallet",
            Stage::Keys => "Generating keys",
            Stage::GenesisFiles => "Writing the genesis",
            Stage::Configs => "Configuring the nodes",
            Stage::Agent => "Registering the background item",
            Stage::Genesis => "Starting the shard (genesis)",
            Stage::Gaze => "Pointing F1R3Gaze at the shard",
            Stage::Opened => "Opening F1R3Gaze",
        }
    }
    fn done(self, s: &Stages) -> bool {
        match self {
            Stage::Preflight => s.preflight,
            Stage::Ports => s.ports,
            Stage::Wallet => s.wallet,
            Stage::Keys => s.keys,
            Stage::GenesisFiles => s.genesis_files,
            Stage::Configs => s.configs,
            Stage::Agent => s.agent,
            Stage::Genesis => s.genesis,
            Stage::Gaze => s.gaze,
            Stage::Opened => s.opened,
        }
    }
    fn mark(self, s: &mut Stages) {
        let f = match self {
            Stage::Preflight => &mut s.preflight,
            Stage::Ports => &mut s.ports,
            Stage::Wallet => &mut s.wallet,
            Stage::Keys => &mut s.keys,
            Stage::GenesisFiles => &mut s.genesis_files,
            Stage::Configs => &mut s.configs,
            Stage::Agent => &mut s.agent,
            Stage::Genesis => &mut s.genesis,
            Stage::Gaze => &mut s.gaze,
            Stage::Opened => &mut s.opened,
        };
        *f = true;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StageStatus {
    Pending,
    Running(String),
    Done,
    Failed(String),
    /// Waiting for the person (background-item approval).
    Waiting(String),
}

pub trait Progress: Send + Sync {
    fn update(&self, stage: Stage, status: StageStatus);
}

pub struct Quiet;
impl Progress for Quiet {
    fn update(&self, stage: Stage, status: StageStatus) {
        match &status {
            StageStatus::Running(d) if !d.is_empty() => info!("{}: {d}", stage.title()),
            StageStatus::Failed(e) => info!("{}: failed: {e}", stage.title()),
            StageStatus::Waiting(w) => info!("{}: {w}", stage.title()),
            _ => {}
        }
        if std::env::var_os("IGN1T10N_QUIET").is_none() {
            let s = match status {
                StageStatus::Pending => return,
                StageStatus::Running(d) => format!("…  {} {d}", stage.title()),
                StageStatus::Done => format!("✓  {}", stage.title()),
                StageStatus::Failed(e) => format!("✗  {}: {e}", stage.title()),
                StageStatus::Waiting(w) => format!("…  {}: {w}", stage.title()),
            };
            eprintln!("{s}");
        }
    }
}

/// Footprint per node, used for S0 and the panel's estimates. PROVISIONAL:
/// Obligation "Footprint" has T1 measure these at the pinned node version and
/// replace them.
pub const NODE_MEMORY_BYTES: u64 = 700 * 1024 * 1024;
pub const NODE_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const EMBERS_MEMORY_BYTES: u64 = 150 * 1024 * 1024;

pub fn memory_needed(validators: u8, embers: bool) -> u64 {
    (validators as u64 + 2) * NODE_MEMORY_BYTES + if embers { EMBERS_MEMORY_BYTES } else { 0 }
}
pub fn disk_needed(validators: u8) -> u64 {
    (validators as u64 + 2) * NODE_DISK_BYTES + 2 * 1024 * 1024 * 1024
}

fn skeleton(c: &Choices) -> Manifest {
    Manifest {
        schema: SCHEMA,
        shard: Shard { id: crate::SHARD_ID.into(), network_id: format!("ign1t10n-{}", hex::encode(crate::random_bytes::<4>())), genesis_validators: c.validators, created: crate::now_rfc3339(), ..Default::default() },
        options: Options { embers: c.embers, gaze_integration: c.gaze_integration, ..Options::default() },
        bootstrap: Keyed::default(),
        observer: Observer::default(),
        faucet: Account::default(),
        wallet: Wallet::default(),
        embers: None,
        validators: vec![],
        resize: None,
        stages: Stages::default(),
    }
}

fn preflight(p: &Paths, m: &Manifest) -> Result<(), String> {
    if let Some(v) = crate::platform::macos_version() {
        if v < (13, 0) {
            return Err(format!("macOS 13 or later is required (this Mac runs {}.{})", v.0, v.1));
        }
    }
    ensure_gaze(p)?;
    crate::platform::check_gaze_signature()?;
    if !p.node_bin.exists() {
        return Err(format!("the node binary is missing ({})", p.node_bin.display()));
    }
    let n = m.shard.genesis_validators;
    p.ensure().map_err(|e| format!("cannot create {}: {e}", p.state.display()))?;
    // Test harnesses only; the menu bar never sets it.
    let skip = std::env::var_os("IGN1T10N_SKIP_RESOURCE_CHECK").is_some();
    if let Some(free) = crate::platform::free_disk(&p.state).filter(|_| !skip) {
        if free < disk_needed(n) {
            return Err(format!("{} GB free; {n} validators need at least {} GB", free >> 30, disk_needed(n) >> 30));
        }
    }
    if let Some(mem) = crate::platform::physical_memory().filter(|_| !skip) {
        if mem < memory_needed(n, m.options.embers) {
            return Err(format!("{} GB of memory; {n} validators need at least {} GB", mem >> 30, (memory_needed(n, m.options.embers) + (1 << 30) - 1) >> 30));
        }
    }
    Ok(())
}

/// F1R3Gaze is part of what ign1t10n installs. The package installs it first;
/// when it is missing anyway (the app was copied without it, or F1R3Gaze was
/// deleted since), install the copy ign1t10n carries.
pub fn ensure_gaze(p: &Paths) -> Result<(), String> {
    if p.gaze_bin().exists() {
        return Ok(());
    }
    if p.gaze_override.is_some() {
        return Err(format!("IGN1T10N_GAZE_BIN names {}, which does not exist", p.gaze_bin().display()));
    }
    if !p.bundled_gaze.exists() {
        return Err(format!(
            "F1R3Gaze is not installed and this copy of ign1t10n does not carry it ({} is missing). \
             Install ign1t10n from its package, or for development set IGN1T10N_GAZE_BIN to a f1r3gaze executable.",
            p.bundled_gaze.display()
        ));
    }
    let installed = crate::platform::install_app(&p.bundled_gaze, &Paths::gaze_app_locations())?;
    info!("installed F1R3Gaze at {}", installed.display());
    if p.gaze_bin().exists() { Ok(()) } else { Err(format!("F1R3Gaze was copied to {} but its executable is missing", installed.display())) }
}

fn allocate_ports(m: &mut Manifest) -> Result<(), String> {
    let mut taken = vec![];
    let take = |pref: u16, taken: &mut Vec<u16>| -> Result<u16, String> {
        let b = ports::allocate(pref, taken).ok_or("no free ports on 127.0.0.1")?;
        taken.extend(b.all());
        Ok(b.0)
    };
    m.bootstrap.base_port = take(ports::BOOTSTRAP_BASE, &mut taken)?;
    m.observer.base_port = take(ports::OBSERVER_BASE, &mut taken)?;
    m.validators = (1..=m.shard.genesis_validators)
        .map(|s| Ok(Validator { slot: s, public_key: String::new(), address: String::new(), base_port: take(ports::validator_base(s), &mut taken)?, state: SlotState::Active, genesis: true, deploy: None, balance_before_payout: None }))
        .collect::<Result<_, String>>()?;
    if m.options.embers {
        let port = ports::allocate_single(ports::EMBERS_PORT, &taken).ok_or("no free port for Embers")?;
        m.embers = Some(Embers { port, ..Default::default() });
    }
    Ok(())
}

fn new_key(p: &Paths, s: &dyn Secrets, name: &str) -> Result<Key, String> {
    let k = Key::generate();
    let pw = crate::keys::new_password();
    s.set(&secrets::pem_account(name), &pw)?;
    k.write(&p.key(name), &pw)?;
    Ok(k)
}

fn keys(p: &Paths, s: &dyn Secrets, m: &mut Manifest) -> Result<(), String> {
    let b = new_key(p, s, "bootstrap")?;
    m.bootstrap.public_key = b.public_hex();
    m.bootstrap.address = b.address();
    let f = new_key(p, s, "faucet")?;
    m.faucet = Account { public_key: f.public_hex(), address: f.address() };
    for v in m.validators.iter_mut() {
        let k = new_key(p, s, &nodeconf::validator_name(v.slot))?;
        v.public_key = k.public_hex();
        v.address = k.address();
    }
    if m.options.embers {
        let mut mm = m.clone();
        *m = configure_embers(p, s, &mut mm)?;
        if let Some(e) = m.embers.as_mut() {
            e.funded = true; // funded at genesis (S4)
        }
    }
    let all: Vec<&str> = std::iter::once(m.bootstrap.public_key.as_str()).chain(m.validators.iter().map(|v| v.public_key.as_str())).collect();
    if all.iter().any(|k| crate::keys::DEVELOPMENT_PUBLIC_KEYS.contains(k)) {
        return Err("generated a development key; refusing".into());
    }
    Ok(())
}

/// Give the manifest an Embers section with fresh secrets (Embers turned on
/// at install or later). Funding happens at genesis or from the faucet.
pub fn configure_embers(_p: &Paths, s: &dyn Secrets, m: &mut Manifest) -> Result<Manifest, String> {
    let e = crate::embers::load_or_create(s)?;
    let (svc, tsvc) = crate::embers::accounts(&e)?;
    let port = match &m.embers {
        Some(x) if x.port != 0 => x.port,
        _ => ports::allocate_single(ports::EMBERS_PORT, &m.taken_ports()).ok_or("no free port for Embers")?,
    };
    let funded = m.embers.as_ref().map(|x| x.funded && x.service == svc).unwrap_or(false);
    m.embers = Some(Embers { port, service: svc, testnet_service: tsvc, funded, validator_slot: None });
    Ok(m.clone())
}

/// Wait for the supervisor to finish the genesis start.
fn await_genesis(p: &Paths, progress: &dyn Progress) -> Result<(), String> {
    let t0 = Instant::now();
    let mut last = String::new();
    loop {
        match control::call(p, &Request::Status, Duration::from_secs(10)) {
            Ok(r) => {
                if let Some(st) = r.status {
                    match st.shard {
                        ShardState::Running | ShardState::Degraded(_) if st.genesis_hash.is_some() => return Ok(()),
                        ShardState::Failed(e) => return Err(e),
                        s => {
                            let l = s.label();
                            if l != last {
                                progress.update(Stage::Genesis, StageStatus::Running(format!("{l} ({}s)", t0.elapsed().as_secs())));
                                last = l;
                            }
                        }
                    }
                }
            }
            Err(e) if t0.elapsed() > Duration::from_secs(60) => return Err(e),
            Err(_) => {}
        }
        let cap = std::env::var("IGN1T10N_START_TIMEOUT_SECS").ok().and_then(|v| v.parse::<u64>().ok()).map(|c| 3 * c).unwrap_or(1200);
        if t0.elapsed() > Duration::from_secs(cap) {
            return Err(format!("the shard did not finish genesis within {cap} seconds"));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub struct Run<'a> {
    pub paths: &'a Paths,
    pub secrets: &'a dyn Secrets,
    pub choices: Choices,
    pub progress: &'a dyn Progress,
    /// `ctl provision --non-interactive`: launch the supervisor directly
    /// instead of registering the agent (S6), and do not open the browser.
    pub headless: bool,
    /// Open F1R3Gaze at the end even when headless.
    pub open: bool,
}

/// Run (or resume) provisioning.
pub fn provision(r: &Run) -> Result<Manifest, String> {
    let p = r.paths;
    let mut m = match Manifest::load(p)? {
        Some(m) => m,
        None => skeleton(&r.choices),
    };
    for stage in Stage::ALL {
        if stage.done(&m.stages) {
            r.progress.update(stage, StageStatus::Done);
            continue;
        }
        r.progress.update(stage, StageStatus::Running(String::new()));
        let res: Result<(), String> = (|| {
            match stage {
                Stage::Preflight => preflight(p, &m),
                Stage::Ports => allocate_ports(&mut m),
                Stage::Wallet => {
                    m.wallet.funded = Some(crate::gaze::paying_wallet(p)?);
                    Ok(())
                }
                Stage::Keys => keys(p, r.secrets, &mut m),
                Stage::GenesisFiles => crate::genesis::write(p, &m),
                Stage::Configs => nodeconf::write_all(p, &m),
                Stage::Agent => {
                    m.stages.agent = false;
                    m.save(p)?;
                    if r.headless {
                        crate::platform::launch_supervisor_directly(p)
                    } else {
                        crate::platform::register_agent(&|w| r.progress.update(Stage::Agent, StageStatus::Waiting(w.into())))
                    }?;
                    // The supervisor answers once it runs.
                    crate::admin::wait_for("the supervisor", Duration::from_secs(60), Duration::from_millis(500), || control::call(p, &Request::Status, Duration::from_secs(5)).ok().map(|_| ()))?;
                    // It saw an unprepared manifest at launch only if it raced us; ask it to start.
                    let _ = control::call(p, &Request::Start, Duration::from_secs(5));
                    Ok(())
                }
                Stage::Genesis => {
                    await_genesis(p, r.progress)?;
                    m = Manifest::load(p)?.ok_or("shard.toml disappeared")?;
                    Ok(())
                }
                Stage::Gaze => {
                    m = Manifest::load(p)?.ok_or("shard.toml disappeared")?;
                    m.options.gaze_integration = r.choices.gaze_integration;
                    crate::gaze::refresh(p, &mut m)
                }
                Stage::Opened => {
                    if !r.headless || r.open {
                        crate::platform::open_gaze()?;
                    }
                    Ok(())
                }
            }
        })();
        match res {
            Ok(()) => {
                if let Ok(Some(disk)) = Manifest::load(p) {
                    // Keep what the supervisor wrote (node id, genesis hash).
                    m.bootstrap.node_id = disk.bootstrap.node_id.or(m.bootstrap.node_id.take());
                    m.shard.genesis_hash = disk.shard.genesis_hash.or(m.shard.genesis_hash.take());
                    m.stages.genesis |= disk.stages.genesis;
                }
                stage.mark(&mut m.stages);
                m.save(p)?;
                r.progress.update(stage, StageStatus::Done);
            }
            Err(e) => {
                r.progress.update(stage, StageStatus::Failed(e.clone()));
                return Err(format!("{}: {e}", stage.title()));
            }
        }
    }
    Ok(m)
}

/// A new shard of `n0` validators with the same options and wallet, after the
/// old one was archived (reset, "Start a new shard with M validators",
/// incompatible upgrade). The supervisor then performs the genesis start.
pub fn reprovision(p: &Paths, s: &dyn Secrets, old: &Manifest, n0: u8) -> Result<Manifest, String> {
    let c = Choices { validators: n0, embers: old.options.embers, gaze_integration: old.options.gaze_integration };
    let mut m = skeleton(&c);
    m.options = old.options.clone();
    m.wallet.funded = old.wallet.funded.clone();
    allocate_ports(&mut m)?;
    keys(p, s, &mut m)?;
    crate::genesis::write(p, &m)?;
    nodeconf::write_all(p, &m)?;
    m.stages = Stages { preflight: true, ports: true, wallet: true, keys: true, genesis_files: true, configs: true, agent: true, genesis: false, gaze: old.stages.gaze, opened: true };
    m.save(p)?;
    Ok(m)
}
