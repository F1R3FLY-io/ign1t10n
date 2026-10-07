//! F1R3Games (spec v0.4 §10): the portal service and the games' clients,
//! served to any web browser at `http://localhost`, with the portal and game
//! environments installed on the local shard and the games registered by a
//! local stand-in for the F1R3FLY.io Cooperative.
//!
//! This module holds what is not process supervision: the secrets
//! (`games.secrets`), the bundle's description (`Resources/f1r3games/
//! games.toml`), the portal's configuration, the environments handed to the
//! portal and to its jobs, and the job runner. The supervisor drives the
//! stages G1..G7 (`supervisor/games.rs`).
//!
//! Every key reaches a process only in its own environment (F1R3Games work
//! package F1), and only the keys that process needs: the portal gets the
//! service key, the environment key and the token secret; the Cooperative's
//! key goes only to the registration job, the breeder's only to the breeder.

use crate::keys::Key;
use crate::logging::MB;
use crate::manifest::{Game, Games, Manifest};
use crate::paths::Paths;
use crate::secrets::Secrets;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const ACCOUNT: &str = "games.secrets";

/// Every game F1R3Games defines, in its crate's order (the port order).
pub const GAME_IDS: [&str; 5] = ["f1r3pix", "f1r3beat", "f1r3ink", "f1r3sidechat", "f1r3skein"];

/// Faucet amounts (whole F1R3): the service key pays the environment deploys
/// and the portal's faucet; the Cooperative and the breeder pay their own deploys.
pub const SERVICE_F1R3: i64 = 1_000_000;
pub const COOP_F1R3: i64 = 1_000;
pub const BREEDER_F1R3: i64 = 1_000;
/// Per new portal key (Decision 14).
pub const DEFAULT_FAUCET_F1R3: i64 = 100;
/// How long a job may wait for its deploys to read back.
pub const JOB_WAIT_SECS: u64 = 300;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct GamesSecrets {
    pub service_key: String,
    pub env_key: String,
    pub token_secret: String,
    pub coop_key: String,
    pub breeder_key: String,
    /// One environment key per game, by id.
    pub game_keys: BTreeMap<String, String>,
}

impl GamesSecrets {
    pub fn generate() -> GamesSecrets {
        let k = || Key::generate().secret_hex();
        GamesSecrets {
            service_key: k(),
            env_key: k(),
            token_secret: hex::encode(crate::random_bytes::<32>()),
            coop_key: k(),
            breeder_key: k(),
            game_keys: GAME_IDS.iter().map(|id| (id.to_string(), k())).collect(),
        }
    }

    fn address(hex: &str) -> Result<String, String> {
        Ok(Key::from_secret_hex(hex)?.address())
    }
    pub fn service_address(&self) -> Result<String, String> {
        Self::address(&self.service_key)
    }
    pub fn coop_address(&self) -> Result<String, String> {
        Self::address(&self.coop_key)
    }
    pub fn breeder_address(&self) -> Result<String, String> {
        Self::address(&self.breeder_key)
    }
}

/// The secrets, created on first use and kept across resets and upgrades
/// (Principle "Keys outlive chains"): the environment URIs, every pinned
/// browser keystore and every manifest hash depend on them.
pub fn load_or_create(s: &dyn Secrets) -> Result<GamesSecrets, String> {
    if let Some(t) = s.get(ACCOUNT)? {
        let mut g: GamesSecrets = serde_json::from_str(&t).map_err(|e| format!("games secrets: {e}"))?;
        // A newer F1R3Games may define a game an older install lacks a key for.
        let missing: Vec<&str> = GAME_IDS.iter().copied().filter(|id| !g.game_keys.contains_key(*id)).collect();
        if !missing.is_empty() {
            for id in missing {
                g.game_keys.insert(id.into(), Key::generate().secret_hex());
            }
            s.set(ACCOUNT, &serde_json::to_string(&g).unwrap())?;
        }
        return Ok(g);
    }
    let g = GamesSecrets::generate();
    s.set(ACCOUNT, &serde_json::to_string(&g).unwrap())?;
    Ok(g)
}

/// What a release bundles: `Resources/f1r3games/games.toml`, written by the
/// release workflow from the pinned F1R3Games revision.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Bundle {
    pub revision: String,
    /// The portal environment's version.
    pub env_version: i64,
    #[serde(rename = "game", default)]
    pub games: Vec<BundleGame>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct BundleGame {
    pub id: String,
    pub env_version: i64,
    /// A web client is bundled at `games/<id>/` (Decision 11: only these are registered).
    #[serde(default)]
    pub client: bool,
}

/// The bundle's F1R3Games, or why there is none (a build without it).
pub fn bundle(p: &Paths) -> Result<Bundle, String> {
    if !p.games_bin.exists() {
        return Err(format!("this build carries no F1R3Games service ({})", p.games_bin.display()));
    }
    let f = p.games_res.join("games.toml");
    let t = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
    let mut b: Bundle = toml::from_str(&t).map_err(|e| format!("{}: {e}", f.display()))?;
    for g in b.games.iter_mut() {
        // A client is bundled only if its files are there.
        g.client = g.client && p.games_res.join("games").join(&g.id).join("index.html").exists();
    }
    Ok(b)
}

/// G1 and G2: the manifest's `games` section, with the addresses of the
/// keys and the ports of the portal and of each bundled client. Ports
/// already recorded are kept (Principle "Stable origins").
pub fn prepare(m: &mut Manifest, sec: &GamesSecrets, b: &Bundle) -> Result<(), String> {
    let mut g = m.games.clone().unwrap_or(Games { faucet_f1r3: DEFAULT_FAUCET_F1R3, ..Default::default() });
    g.service_address = sec.service_address()?;
    g.coop_address = sec.coop_address()?;
    g.breeder_address = sec.breeder_address()?;
    g.stages.secrets = true;
    for id in GAME_IDS {
        if g.game(id).is_none() {
            g.game.push(Game { id: id.into(), ..Default::default() });
        }
    }
    for bg in &b.games {
        if let Some(x) = g.game_mut(&bg.id) {
            x.client = bg.client;
        }
    }
    if g.revision.is_empty() {
        g.revision = b.revision.clone();
    }
    if !g.stages.ports || g.portal_port == 0 {
        let ipv6 = crate::ports::ipv6_loopback();
        // Ports recorded before (a move) stay taken, so the new origins differ.
        let taken = m.taken_ports();
        let base = crate::ports::allocate_games(GAME_IDS.len(), &taken, ipv6).ok_or("no free ports for F1R3Games")?;
        g.ipv6 = ipv6;
        g.portal_port = base;
        for (i, id) in GAME_IDS.iter().enumerate() {
            let x = g.game_mut(id).unwrap();
            x.port = x.client.then_some(base + 1 + i as u16);
        }
        g.stages.ports = true;
    } else {
        // A client that arrived in a later bundle gets its reserved port.
        let base = g.portal_port;
        for (i, id) in GAME_IDS.iter().enumerate() {
            let x = g.game_mut(id).unwrap();
            if x.client && x.port.is_none() {
                x.port = Some(base + 1 + i as u16);
            }
        }
    }
    m.games = Some(g);
    Ok(())
}

fn listen(port: u16, ipv6: bool) -> toml::Value {
    let mut v = vec![toml::Value::String(format!("127.0.0.1:{port}"))];
    if ipv6 {
        v.push(toml::Value::String(format!("[::1]:{port}")));
    }
    toml::Value::Array(v)
}

/// The portal's configuration (spec v0.4 Appendix A.2). `validators` are the
/// deploy targets (F7: one is drawn per deploy); `observer` the reads.
pub fn render_config(p: &Paths, g: &Games, validators: &[String], observer: &str, shard_id: &str, env_version: i64) -> String {
    use toml::Value as V;
    let mut t = toml::Table::new();
    t.insert("listen".into(), listen(g.portal_port, g.ipv6));
    t.insert("public_host".into(), V::String(format!("localhost:{}", g.portal_port)));
    t.insert("shard_id".into(), V::String(shard_id.into()));
    t.insert("validator_url".into(), V::String(validators.first().cloned().unwrap_or_default()));
    t.insert("validator_urls".into(), V::Array(validators.iter().cloned().map(V::String).collect()));
    t.insert("observer_url".into(), V::String(observer.into()));
    t.insert("coop_address".into(), V::String(g.coop_address.clone()));
    t.insert("env_version".into(), V::Integer(env_version));
    t.insert("phlo_price".into(), V::Integer(crate::amounts::PHLO_PRICE));
    t.insert("bootstrap_env".into(), V::Boolean(false));
    t.insert("cors_origins".into(), V::Array(vec![]));
    t.insert("portal_base_url".into(), V::String(g.url()));
    t.insert("static_dir".into(), V::String(p.games_res.join("portal").display().to_string()));
    let mut faucet = toml::Table::new();
    faucet.insert("enabled".into(), V::Boolean(true));
    faucet.insert("amount".into(), V::Integer(crate::amounts::dust(g.faucet_f1r3)));
    t.insert("faucet".into(), V::Table(faucet));
    let origins: Vec<V> = g
        .served()
        .iter()
        .map(|x| {
            let port = x.port.unwrap();
            let mut o = toml::Table::new();
            o.insert("id".into(), V::String(x.id.clone()));
            o.insert("listen".into(), listen(port, g.ipv6));
            o.insert("public_host".into(), V::String(format!("localhost:{port}")));
            o.insert("dir".into(), V::String(p.games_res.join("games").display().to_string()));
            o.insert("frame_ancestors".into(), V::Array(vec![V::String(g.url())]));
            V::Table(o)
        })
        .collect();
    t.insert("origins".into(), V::Array(origins));
    format!("# Rendered by ign1t10n at every start. Keys come from the environment.\n{}", toml::to_string_pretty(&t).unwrap())
}

/// F1R3Games after a reset: keys and origins kept, nothing on the new chain yet.
pub fn after_reset(g: &Games) -> Games {
    let mut x = g.clone();
    x.stages = g.stages.after_reset();
    x.env_version = 0;
    x.validator_slot = None;
    x.nursery = None;
    x.last_epoch = None;
    x.pending_update = None;
    for gm in x.game.iter_mut() {
        gm.env_version = 0;
        gm.registered = false;
        gm.manifest_sha256 = None;
    }
    x
}

/// The game's entry base: its own origin.
pub fn entry_base(port: u16) -> String {
    format!("http://localhost:{port}")
}

/// `/api/ready?games=a,b` for the registered games.
pub fn ready_path(g: &Games) -> String {
    let ids: Vec<&str> = g.served().iter().map(|x| x.id.as_str()).collect();
    format!("/api/ready?games={}", ids.join(","))
}

fn game_var(id: &str) -> String {
    format!("F1R3GAMES_GAME_KEY_{}", id.to_ascii_uppercase().replace('-', "_"))
}

/// Which keys a process receives.
#[derive(Default, Clone, Copy)]
pub struct Grant {
    pub portal: bool,
    pub game_keys: bool,
    pub coop: bool,
}

pub fn env(sec: &GamesSecrets, grant: Grant) -> Vec<(String, String)> {
    let mut e = crate::procs::base_env();
    e.push(("RUST_LOG".into(), "info".into()));
    if grant.portal {
        e.push(("F1R3GAMES_SERVICE_KEY".into(), sec.service_key.clone()));
        e.push(("F1R3GAMES_ENV_KEY".into(), sec.env_key.clone()));
        e.push(("F1R3GAMES_TOKEN_SECRET".into(), sec.token_secret.clone()));
    }
    if grant.game_keys {
        for (id, k) in &sec.game_keys {
            e.push((game_var(id), k.clone()));
        }
    }
    if grant.coop {
        e.push(("F1R3GAMES_COOP_KEY".into(), sec.coop_key.clone()));
    }
    e
}

/// The environment for the headless CLI (F1R3Games F6): one key.
pub fn cli_env(p: &Paths, key_hex: &str, service: &str) -> Vec<(String, String)> {
    let mut e = crate::procs::base_env();
    e.push(("F1R3GAMES_KEY".into(), key_hex.into()));
    e.push(("F1R3GAMES_SERVICE".into(), service.into()));
    e.push(("F1R3GAMES_HOME".into(), p.games().join("cli").display().to_string()));
    e
}

/// Run one job of `program` to completion, logging its output to
/// `games-job.log`. Its standard output is returned.
pub fn run_job(p: &Paths, program: &std::path::Path, args: &[String], env: &[(String, String)], timeout: Duration) -> Result<String, String> {
    let log = p.log_file("games-job");
    rotate(&log);
    let mut l = std::fs::OpenOptions::new().create(true).append(true).open(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let _ = writeln!(l, "--- {} {} {}", crate::now_rfc3339(), program.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(), args.join(" "));
    let mut c = Command::new(program);
    c.args(args).env_clear().envs(env.iter().cloned()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("{}: {e}", program.display()))?;
    // Drain both pipes on threads so a chatty job cannot block on a full pipe.
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut so, &mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut se, &mut b);
        b
    });
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writeln!(l, "(killed after {}s)", timeout.as_secs());
                return Err(format!("{} {} did not finish within {}s", program.display(), args.first().cloned().unwrap_or_default(), timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).to_string();
    let err = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).to_string();
    let _ = l.write_all(out.as_bytes());
    let _ = l.write_all(err.as_bytes());
    if status.success() {
        Ok(out)
    } else {
        let tail: Vec<&str> = err.lines().rev().filter(|x| !x.trim().is_empty()).take(3).collect();
        Err(format!("{} {} failed ({status}): {}", program.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(), args.first().cloned().unwrap_or_default(), tail.into_iter().rev().collect::<Vec<_>>().join(" | ")))
    }
}

fn rotate(log: &std::path::Path) {
    if std::fs::metadata(log).map(|m| m.len() > 10 * MB).unwrap_or(false) {
        let _ = std::fs::rename(log, log.with_extension("log.1"));
    }
}

/// One game's entry in a manifests file, and the SHA-256 of what decides its
/// behaviour (the whole typed manifest).
pub fn manifest_entries(text: &str) -> Result<Vec<(String, String)>, String> {
    use sha2::Digest;
    let list: Vec<serde_json::Value> = serde_json::from_str(text).map_err(|e| format!("manifests: {e}"))?;
    Ok(list
        .iter()
        .filter_map(|e| {
            let id = e["id"].as_str()?.to_string();
            Some((id, hex::encode(sha2::Sha256::digest(e["manifest"].to_string().as_bytes()))))
        })
        .collect())
}

/// From `f1r3games-service status`: each game environment's URI and version.
pub fn env_uris(status: &serde_json::Value) -> BTreeMap<String, (String, Option<i64>)> {
    let mut out = BTreeMap::new();
    if let Some(gs) = status["games"].as_object() {
        for (id, g) in gs {
            if let Some(uri) = g["env"]["uri"].as_str() {
                out.insert(id.clone(), (uri.to_string(), g["env"]["version"].as_i64()));
            }
        }
    }
    out
}

/// What an upgrade of the bundle asks of the chain, compared with the
/// manifest: games whose environment version rose, and whether the portal
/// environment's did. Both discard on-chain state, so both need consent.
pub fn pending(g: &Games, b: &Bundle) -> Option<String> {
    let mut what = vec![];
    if g.stages.portal_env && b.env_version > g.env_version {
        what.push(format!("the portal environment ({} → {}; discards profiles, instances, plays, invitations and sponsorships)", g.env_version, b.env_version));
    }
    for bg in &b.games {
        if let Some(x) = g.game(&bg.id) {
            if g.stages.game_envs && x.env_version > 0 && bg.env_version > x.env_version {
                what.push(format!("{} ({} → {}; replaces its live instances)", bg.id, x.env_version, bg.env_version));
            }
        }
    }
    (!what.is_empty()).then(|| what.join("; "))
}

/// A first-run nursery for the F1R3Beat breeder: public, default shape.
pub const NURSERY_CONFIG: &str = r#"{"map":{"meter":[4,4],"bars":2,"column":[1,16],"capacity":32,"seating":"random","scale":null,"tempo":100,"seed":null,"messageLimit":2048}}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests_support::sample;

    fn paths(d: &std::path::Path) -> Paths {
        let mut p = Paths::from_env();
        p.state = d.join("state");
        p.games_res = d.join("res");
        p.games_bin = d.join("f1r3games-service");
        p
    }

    fn bundle_on_disk(p: &Paths) -> Bundle {
        std::fs::write(&p.games_bin, "").unwrap();
        for id in ["f1r3pix", "f1r3beat"] {
            std::fs::create_dir_all(p.games_res.join("games").join(id)).unwrap();
            std::fs::write(p.games_res.join("games").join(id).join("index.html"), "x").unwrap();
        }
        std::fs::write(
            p.games_res.join("games.toml"),
            "revision = \"57a3b27\"\nenv_version = 1\n[[game]]\nid = \"f1r3pix\"\nenv_version = 2\nclient = true\n[[game]]\nid = \"f1r3beat\"\nenv_version = 1\nclient = true\n[[game]]\nid = \"f1r3ink\"\nenv_version = 1\nclient = true\n",
        )
        .unwrap();
        bundle(p).unwrap()
    }

    #[test]
    fn secrets_persist_and_grow_with_new_games() {
        let d = tempfile::tempdir().unwrap();
        let s = crate::secrets::FileSecrets { path: d.path().join("s.json") };
        let a = load_or_create(&s).unwrap();
        assert_eq!(a.game_keys.len(), GAME_IDS.len());
        assert_eq!(load_or_create(&s).unwrap(), a, "kept, not regenerated");
        let mut old = a.clone();
        old.game_keys.remove("f1r3skein");
        s.set(ACCOUNT, &serde_json::to_string(&old).unwrap()).unwrap();
        let b = load_or_create(&s).unwrap();
        assert_eq!(b.service_key, a.service_key);
        assert!(b.game_keys.contains_key("f1r3skein"));
    }

    #[test]
    fn a_client_counts_only_when_its_files_are_there() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        let b = bundle_on_disk(&p);
        assert!(b.games.iter().find(|g| g.id == "f1r3pix").unwrap().client);
        assert!(!b.games.iter().find(|g| g.id == "f1r3ink").unwrap().client, "listed, but no index.html");
    }

    #[test]
    fn prepare_allocates_once_and_keeps_the_origins() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        let b = bundle_on_disk(&p);
        let sec = GamesSecrets::generate();
        let mut m = sample();
        prepare(&mut m, &sec, &b).unwrap();
        let g = m.games.clone().unwrap();
        assert_eq!(g.portal_port % 10, 0);
        assert_eq!(g.game("f1r3pix").unwrap().port, Some(g.portal_port + 1));
        assert_eq!(g.game("f1r3beat").unwrap().port, Some(g.portal_port + 2));
        assert_eq!(g.game("f1r3ink").unwrap().port, None);
        assert_eq!(g.coop_address, sec.coop_address().unwrap());
        // Again: nothing moves.
        prepare(&mut m, &sec, &b).unwrap();
        assert_eq!(m.games.as_ref().unwrap().portal_port, g.portal_port);
        assert!(m.taken_ports().contains(&g.portal_port));
    }

    #[test]
    fn the_portal_configuration_reads_back() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        let b = bundle_on_disk(&p);
        let mut m = sample();
        prepare(&mut m, &GamesSecrets::generate(), &b).unwrap();
        let mut g = m.games.unwrap();
        g.ipv6 = true;
        let t = render_config(&p, &g, &["http://127.0.0.1:40413".into(), "http://127.0.0.1:40423".into()], "http://127.0.0.1:40453", "root", 1);
        let v: toml::Table = toml::from_str(&t).unwrap();
        assert_eq!(v["public_host"].as_str().unwrap(), format!("localhost:{}", g.portal_port));
        assert_eq!(v["listen"].as_array().unwrap().len(), 2);
        assert_eq!(v["validator_urls"].as_array().unwrap().len(), 2);
        assert_eq!(v["origins"].as_array().unwrap().len(), 2);
        assert_eq!(v["faucet"]["amount"].as_integer().unwrap(), 10_000_000_000);
        assert!(!t.contains("_key"), "no key in the file: {t}");
        assert!(!t.contains("0.0.0.0"));
        assert_eq!(ready_path(&g), "/api/ready?games=f1r3pix,f1r3beat");
    }

    #[test]
    fn grants_are_least_privilege() {
        let s = GamesSecrets::generate();
        let names = |e: Vec<(String, String)>| e.into_iter().map(|(k, _)| k).filter(|k| k.starts_with("F1R3GAMES")).collect::<Vec<_>>();
        let portal = names(env(&s, Grant { portal: true, ..Default::default() }));
        assert_eq!(portal, ["F1R3GAMES_SERVICE_KEY", "F1R3GAMES_ENV_KEY", "F1R3GAMES_TOKEN_SECRET"]);
        let reg = names(env(&s, Grant { portal: true, game_keys: true, coop: true }));
        assert!(reg.contains(&"F1R3GAMES_COOP_KEY".to_string()) && reg.contains(&"F1R3GAMES_GAME_KEY_F1R3PIX".to_string()));
    }

    #[test]
    fn an_upgrade_that_discards_state_waits_for_consent() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        let b = bundle_on_disk(&p);
        let mut g = Games { env_version: 1, ..Default::default() };
        g.stages.portal_env = true;
        g.stages.game_envs = true;
        g.game = vec![Game { id: "f1r3pix".into(), env_version: 2, ..Default::default() }, Game { id: "f1r3beat".into(), env_version: 1, ..Default::default() }];
        assert_eq!(pending(&g, &b), None);
        g.game[0].env_version = 1;
        assert!(pending(&g, &b).unwrap().contains("f1r3pix (1 → 2"));
    }

    #[test]
    fn manifests_are_hashed_per_game() {
        let t = r#"[{"id":"f1r3pix","manifest":{"map":{"id":"f1r3pix"}}},{"id":"f1r3beat","manifest":{"map":{"id":"f1r3beat"}}}]"#;
        let e = manifest_entries(t).unwrap();
        assert_eq!(e.len(), 2);
        assert_ne!(e[0].1, e[1].1);
    }

    #[test]
    fn jobs_report_failures_with_their_last_lines() {
        let d = tempfile::tempdir().unwrap();
        let mut p = paths(d.path());
        p.logs = d.path().join("logs");
        std::fs::create_dir_all(&p.logs).unwrap();
        let out = run_job(&p, std::path::Path::new("/bin/sh"), &["-c".into(), "echo hello; echo F1R3GAMES_KEY=$F1R3GAMES_KEY".into()], &[("F1R3GAMES_KEY".into(), "k".into())], Duration::from_secs(10)).unwrap();
        assert!(out.contains("hello") && out.contains("F1R3GAMES_KEY=k"));
        let e = run_job(&p, std::path::Path::new("/bin/sh"), &["-c".into(), "echo boom >&2; exit 3".into()], &[], Duration::from_secs(10)).unwrap_err();
        assert!(e.contains("boom"), "{e}");
        let e = run_job(&p, std::path::Path::new("/bin/sh"), &["-c".into(), "sleep 5".into()], &[], Duration::from_secs(1)).unwrap_err();
        assert!(e.contains("did not finish"), "{e}");
    }
}
