//! End to end on one machine with a fake node (examples/fake_node.rs):
//! headless provisioning (S0..S9 minus the launch agent), genesis, F1R3Gaze
//! settings, crash recovery, a grow and a shrink, and uninstall; and, with
//! the real `f1r3games-service` (`IGN1T10N_TEST_GAMES_BIN`), F1R3Games:
//! G1..G7, the portal and game origins, restarts, off/on and reset.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ign1t10n"))
}

fn fake_node() -> PathBuf {
    let p = exe().parent().unwrap().join("examples/fake_node");
    if !p.exists() {
        assert!(Command::new(env!("CARGO")).args(["build", "--example", "fake_node"]).status().unwrap().success());
    }
    p
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    wallet: String,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let wallet = ign1t10n::keys::Key::generate().address();
        // A stand-in F1R3Gaze CLI: `wallet list` shows one active wallet.
        let gaze = root.join("f1r3gaze");
        std::fs::write(&gaze, format!("#!/bin/sh\ncase \"$*\" in *'wallet list'*) echo '* {wallet}  Local shard';; *) echo 'unexpected' >&2; exit 1;; esac\n")).unwrap();
        ign1t10n::paths::set_mode(&gaze, 0o755).unwrap();
        Env { _dir: dir, root, wallet }
    }

    fn cmd(&self, args: &[&str]) -> (bool, String) {
        self.cmd_env(args, &[])
    }

    fn cmd_env(&self, args: &[&str], extra: &[(&str, String)]) -> (bool, String) {
        let out = Command::new(exe())
            .args(args)
            .envs(extra.iter().map(|(k, v)| (k.to_string(), v.clone())))
            .env("IGN1T10N_STATE_DIR", self.root.join("state"))
            .env("IGN1T10N_LOG_DIR", self.root.join("logs"))
            .env("F1R3GAZE_PROFILE", self.root.join("profile"))
            .env("IGN1T10N_NODE_BIN", fake_node())
            .env("IGN1T10N_EMBERS_BIN", fake_node())
            .env("IGN1T10N_GAZE_BIN", self.root.join("f1r3gaze"))
            .env("IGN1T10N_DEV_SECRETS", "1")
            .env("IGN1T10N_QUIET", "1")
            .env("IGN1T10N_SKIP_RESOURCE_CHECK", "1")
            .env("IGN1T10N_START_TIMEOUT_SECS", "45")
            .output()
            .unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    fn status(&self) -> serde_json::Value {
        let (ok, out) = self.cmd(&["ctl", "status", "--json"]);
        assert!(ok, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    fn wait(&self, what: &str, secs: u64, f: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        let t0 = Instant::now();
        loop {
            let s = self.status();
            if f(&s) {
                return s;
            }
            if t0.elapsed() > Duration::from_secs(secs) {
                panic!("timed out waiting for {what}: {s:#}\n{}", std::fs::read_to_string(self.root.join("logs/ign1t10n.log")).unwrap_or_default());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.cmd(&["ctl", "uninstall", "--yes"]);
    }
}

fn state(s: &serde_json::Value) -> &str {
    s["shard"]["state"].as_str().unwrap_or("")
}

fn pid_of(root: &Path, node: &str) -> Option<i32> {
    let out = Command::new("pgrep").args(["-f", &format!("{}.conf", root.join("state/conf").join(node).display())]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
}

#[test]
fn provision_run_resize_and_uninstall() {
    let e = Env::new();
    let (ok, out) = e.cmd(&["ctl", "provision", "--non-interactive", "--no-games"]);
    let log = || {
        let mut l = std::fs::read_to_string(e.root.join("logs/ign1t10n.log")).unwrap_or_default();
        for n in ["bootstrap", "validator-1", "validator-2", "observer"] {
            l += &format!("\n--- {n}.stdout.log\n{}", std::fs::read_to_string(e.root.join(format!("logs/{n}.stdout.log"))).unwrap_or_default());
        }
        l
    };
    assert!(ok, "provision failed:\n{out}\n{}", log());

    let s = e.wait("running", 60, |s| state(s) == "running");
    assert_eq!(s["validators"], 2);
    assert_eq!(s["genesis_hash"], "9e3a5fake0genesis");
    assert_eq!(s["nodes"].as_array().unwrap().len(), 5, "bootstrap, 2 validators, observer, embers");
    assert!(s["embers_enabled"].as_bool().unwrap(), "Embers is bundled by default");

    // Genesis gave the browser's wallet 10,000,000 F1R3.
    let w = std::fs::read_to_string(e.root.join("state/genesis/wallets.txt")).unwrap();
    assert!(w.starts_with(&format!("{},1000000000000000\n", e.wallet)));

    // F1R3Gaze deploys to a validator (never the bootstrap), reads the observer, uses Embers.
    // Ports come from the status report: when the plan's ports are taken,
    // provisioning falls back (A7), so the test must not assume them.
    let port = |name: &str| s["nodes"].as_array().unwrap().iter().find(|n| n["name"] == name).unwrap()["base_port"].as_u64().unwrap();
    let settings = std::fs::read_to_string(e.root.join("profile/settings.conf")).unwrap();
    let vs = [port("validator-1") + 3, port("validator-2") + 3];
    assert!(vs.iter().any(|p| settings.contains(&format!("validator = http://127.0.0.1:{p}\n"))), "{settings}");
    assert!(!settings.contains(&format!("validator = http://127.0.0.1:{}", port("bootstrap") + 3)));
    assert!(settings.contains(&format!("observers = http://127.0.0.1:{}", port("observer") + 3)));
    assert!(settings.contains(&format!("embers_api = http://127.0.0.1:{}", port("embers"))));

    // No key or password on any command line.
    let ps = String::from_utf8_lossy(&Command::new("ps").args(["-eo", "args"]).output().unwrap().stdout).to_string();
    assert!(!ps.contains("--validator-private-key "));
    assert!(!ps.contains("F1R3NODE_VALIDATOR_PASSWORD"));

    // A8: a crashed validator comes back.
    let pid = pid_of(&e.root, "validator-2").expect("validator-2 runs");
    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(Duration::from_secs(2));
    e.wait("validator-2 back", 30, |_| pid_of(&e.root, "validator-2").is_some_and(|p| p != pid));

    // A14: bounds.
    let (ok, out) = e.cmd(&["ctl", "resize", "11"]);
    assert!(!ok && out.contains("between 2 and 10"), "{out}");

    // A11: grow in place.
    let (ok, out) = e.cmd(&["ctl", "resize", "3"]);
    assert!(ok, "{out}");
    let s = e.wait("grown to 3", 120, |s| state(s) == "running" && s["validators"] == 3 && s["resize"].is_null());
    assert!(s["nodes"].as_array().unwrap().iter().any(|n| n["name"] == "validator-3" && n["state"] == "active"));

    // A12: shrink in place; validator 1 untouched, the slot archived.
    let (ok, out) = e.cmd(&["ctl", "resize", "2"]);
    assert!(ok, "{out}");
    e.wait("shrunk to 2", 120, |s| state(s) == "running" && s["validators"] == 2 && s["resize"].is_null());
    assert!(pid_of(&e.root, "validator-3").is_none(), "the withdrawn validator stopped");
    assert!(std::fs::read_dir(e.root.join("state/archive")).unwrap().next().is_some());

    // A10: uninstall leaves nothing, and the managed settings are gone.
    let (ok, out) = e.cmd(&["ctl", "uninstall", "--yes"]);
    assert!(ok, "{out}");
    assert!(!e.root.join("state").exists());
    let settings = std::fs::read_to_string(e.root.join("profile/settings.conf")).unwrap();
    assert!(!settings.contains("managed by ign1t10n"));
    assert!(pid_of(&e.root, "bootstrap").is_none());
}

/// A bundle as the release workflow lays it out, with stand-in clients.
fn games_bundle(root: &Path) -> PathBuf {
    let res = root.join("res");
    for (f, body) in [
        ("portal/index.html", "<!doctype html><title>F1R3Games</title>portal"),
        ("games/f1r3pix/index.html", "pix"),
        ("games/f1r3pix/preview/canvas.html", "canvas"),
        ("games/f1r3beat/index.html", "beat"),
    ] {
        let p = res.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let mut t = String::from("revision = \"test\"\nenv_version = 1\n");
    for (id, v, client) in [("f1r3pix", 2, true), ("f1r3beat", 1, true), ("f1r3ink", 1, false), ("f1r3sidechat", 1, false), ("f1r3skein", 1, false)] {
        t += &format!("[[game]]\nid = \"{id}\"\nenv_version = {v}\nclient = {client}\n");
    }
    std::fs::write(res.join("games.toml"), t).unwrap();
    res
}

fn http(url: &str, host: Option<&str>) -> (u16, String, Option<String>, Option<String>) {
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(10)).build();
    let mut r = agent.get(url);
    if let Some(h) = host {
        r = r.set("host", h);
    }
    let resp = match r.call() {
        Ok(x) => x,
        Err(ureq::Error::Status(_, x)) => x,
        Err(e) => panic!("{url}: {e}"),
    };
    let code = resp.status();
    let loc = resp.header("location").map(str::to_string);
    let csp = resp.header("content-security-policy").map(str::to_string);
    (code, resp.into_string().unwrap_or_default(), loc, csp)
}

fn games_state(s: &serde_json::Value) -> &str {
    s["games"]["state"]["state"].as_str().unwrap_or("")
}

#[test]
fn f1r3games_installs_serves_survives_and_resets() {
    let Some(games_bin) = std::env::var_os("IGN1T10N_TEST_GAMES_BIN").map(PathBuf::from) else {
        eprintln!("skipped: set IGN1T10N_TEST_GAMES_BIN to a built f1r3games-service");
        return;
    };
    let e = Env::new();
    let res = games_bundle(&e.root);
    let opened = e.root.join("opened-url");
    let extra = [
        ("IGN1T10N_GAMES_BIN", games_bin.display().to_string()),
        ("IGN1T10N_GAMES_DIR", res.display().to_string()),
        ("IGN1T10N_OPEN_URL_LOG", opened.display().to_string()),
    ];
    let cmd = |args: &[&str]| e.cmd_env(args, &extra);
    let status = || {
        let (ok, out) = cmd(&["ctl", "status", "--json"]);
        assert!(ok, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()
    };
    let wait = |what: &str, secs: u64, f: &dyn Fn(&serde_json::Value) -> bool| -> serde_json::Value {
        let t0 = Instant::now();
        loop {
            let s = status();
            if f(&s) {
                return s;
            }
            if t0.elapsed() > Duration::from_secs(secs) {
                panic!(
                    "timed out waiting for {what}: {s:#}\n{}\n--- games-job.log\n{}\n--- portal\n{}",
                    std::fs::read_to_string(e.root.join("logs/ign1t10n.log")).unwrap_or_default(),
                    std::fs::read_to_string(e.root.join("logs/games-job.log")).unwrap_or_default(),
                    std::fs::read_to_string(e.root.join("logs/portal.stdout.log")).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    };

    // G1..G7 at first run, the portal opened in "the browser".
    let (ok, out) = cmd(&["ctl", "provision", "--non-interactive", "--no-embers", "--open"]);
    assert!(ok, "provision failed:\n{out}\n{}\n{}", std::fs::read_to_string(e.root.join("logs/ign1t10n.log")).unwrap_or_default(), std::fs::read_to_string(e.root.join("logs/games-job.log")).unwrap_or_default());
    let s = wait("F1R3Games running", 120, &|s| games_state(s) == "running");
    let url = s["games"]["url"].as_str().unwrap().to_string();
    let port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
    assert_eq!(std::fs::read_to_string(&opened).unwrap(), format!("{url}/"));
    let games = s["games"]["games"].as_array().unwrap();
    let pix = games.iter().find(|g| g["id"] == "f1r3pix").unwrap();
    assert_eq!(pix["origin"], format!("http://localhost:{}", port + 1));
    assert!(pix["registered"].as_bool().unwrap());
    assert_eq!(pix["env_version"], 2);
    let ink = games.iter().find(|g| g["id"] == "f1r3ink").unwrap();
    assert!(!ink["registered"].as_bool().unwrap() && ink["origin"].is_null(), "no client, not registered");

    // The portal: its shell and API at localhost only; the games on origins of their own.
    let (code, body, _, _) = http(&format!("http://127.0.0.1:{port}/"), Some(&format!("localhost:{port}")));
    assert_eq!((code, body.contains("portal")), (200, true));
    let (code, _, loc, _) = http(&format!("http://127.0.0.1:{port}/api/health"), None);
    assert_eq!((code, loc.as_deref()), (308, Some(format!("http://localhost:{port}/api/health").as_str())));
    let (code, _, _, _) = http(&format!("http://127.0.0.1:{port}/api/health"), Some("evil.example"));
    assert_eq!(code, 421);
    let (code, body, _, _) = http(&format!("http://127.0.0.1:{port}/api/ready?games=f1r3pix,f1r3beat"), Some(&format!("localhost:{port}")));
    assert_eq!(code, 200, "{body}");
    let (code, body, _, _) = http(&format!("http://127.0.0.1:{port}/api/games/f1r3pix"), Some(&format!("localhost:{port}")));
    assert!(code == 200 && body.contains(&format!("http://localhost:{}/f1r3pix/", port + 1)), "{body}");
    let (code, body, _, csp) = http(&format!("http://127.0.0.1:{}/f1r3pix/", port + 1), Some(&format!("localhost:{}", port + 1)));
    assert_eq!((code, body.as_str(), csp.as_deref()), (200, "pix", Some(format!("frame-ancestors {url}").as_str())));
    let (code, _, _, _) = http(&format!("http://127.0.0.1:{}/f1r3beat/", port + 1), Some(&format!("localhost:{}", port + 1)));
    assert_eq!(code, 404, "one origin, one game");

    // No key on any command line, none in the configuration file.
    let ps = String::from_utf8_lossy(&Command::new("ps").args(["-eo", "args"]).output().unwrap().stdout).to_string();
    assert!(!ps.contains("F1R3GAMES_"), "keys only in environments");
    let conf = std::fs::read_to_string(e.root.join("state/games/f1r3games.toml")).unwrap();
    assert!(!conf.contains("_key") && !conf.contains("token"), "{conf}");

    // A crashed portal comes back; the shard is untouched.
    let pid = |root: &Path| -> Option<i32> {
        let out = Command::new("pgrep").args(["-f", &format!("{} serve", root.join("state/games/f1r3games.toml").display())]).output().ok()?;
        String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
    };
    let p0 = pid(&e.root).expect("the portal runs");
    unsafe { libc::kill(p0, libc::SIGKILL) };
    wait("the portal back", 60, &|_| pid(&e.root).is_some_and(|p| p != p0));
    wait("F1R3Games running again", 60, &|s| games_state(s) == "running" && state(s) == "running");

    // Off frees the origins; on brings them back at the same address, deploying nothing.
    // Registration jobs run (the job log's header line for each).
    let regs = || std::fs::read_to_string(e.root.join("logs/games-job.log")).unwrap_or_default().lines().filter(|l| l.starts_with("--- ") && l.contains(" register-games ")).count();
    let before = regs();
    let (ok, out) = cmd(&["ctl", "games", "off"]);
    assert!(ok, "{out}");
    assert!(ign1t10n::ports::port_free(port) && ign1t10n::ports::port_free(port + 1));
    let (ok, out) = cmd(&["ctl", "games", "on"]);
    assert!(ok, "{out}");
    let s = wait("F1R3Games on again", 120, &|s| games_state(s) == "running");
    assert_eq!(s["games"]["url"], url.as_str(), "the origin never moves");
    assert_eq!(regs(), before, "nothing registered again:\n{}\n{}", std::fs::read_to_string(e.root.join("logs/games-job.log")).unwrap_or_default(), std::fs::read_to_string(e.root.join("logs/ign1t10n.log")).unwrap_or_default());

    // Reset: a new chain, the same keys and origins; everything installed again.
    let env_uri = |root: &Path| -> String {
        let m: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("state/shard.toml")).unwrap()).unwrap();
        m["games"]["env_uri"].as_str().unwrap_or("").to_string()
    };
    let uri0 = env_uri(&e.root);
    assert!(uri0.starts_with("rho:id:"), "{uri0}");
    let (ok, out) = cmd(&["ctl", "reset", "--yes"]);
    assert!(ok, "{out}");
    let s = wait("reset and F1R3Games running", 240, &|s| state(s) == "running" && games_state(s) == "running" && s["genesis_hash"].is_string());
    assert_eq!(s["games"]["url"], url.as_str());
    assert_eq!(env_uri(&e.root), uri0, "keys outlive chains");
    assert!(regs() > before, "registered on the new chain");

    let (ok, out) = cmd(&["ctl", "uninstall", "--yes"]);
    assert!(ok, "{out}");
    assert!(pid(&e.root).is_none());
}
