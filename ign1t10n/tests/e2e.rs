//! End to end on one machine with a fake node (examples/fake_node.rs):
//! headless provisioning (S0..S9 minus the launch agent), genesis, F1R3Gaze
//! settings, crash recovery, a grow and a shrink, and uninstall.

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
        let out = Command::new(exe())
            .args(args)
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
    let (ok, out) = e.cmd(&["ctl", "provision", "--non-interactive"]);
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
