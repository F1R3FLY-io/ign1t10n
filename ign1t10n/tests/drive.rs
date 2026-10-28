//! End to end for sidecars + f1r3drive mounts: provision a two-validator
//! shard with fake nodes, confirm every sidecar is up, add + mount +
//! write-through + read-back + unmount + remove a drive.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ign1t10n"))
}

fn example(name: &str) -> PathBuf {
    let p = exe().parent().unwrap().join(format!("examples/{name}"));
    if !p.exists() {
        assert!(
            Command::new(env!("CARGO")).args(["build", "--example", name]).status().unwrap().success(),
            "cargo build --example {name} failed",
        );
    }
    p
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let wallet = ign1t10n::keys::Key::generate().address();
        // Stand-in F1R3Gaze CLI.
        let gaze = root.join("f1r3gaze");
        std::fs::write(
            &gaze,
            format!("#!/bin/sh\ncase \"$*\" in *'wallet list'*) echo '* {wallet}  Local shard';; *) echo 'unexpected' >&2; exit 1;; esac\n"),
        )
        .unwrap();
        ign1t10n::paths::set_mode(&gaze, 0o755).unwrap();
        Env { _dir: dir, root, home }
    }

    fn cmd(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(exe())
            .args(args)
            .env("HOME", &self.home)
            .env("IGN1T10N_STATE_DIR", self.root.join("state"))
            .env("IGN1T10N_LOG_DIR", self.root.join("logs"))
            .env("F1R3GAZE_PROFILE", self.root.join("profile"))
            .env("IGN1T10N_NODE_BIN", example("fake_node"))
            .env("IGN1T10N_EMBERS_BIN", example("fake_node"))
            .env("IGN1T10N_SIDECAR_BIN", example("fake_sidecar"))
            .env("IGN1T10N_F1R3DRIVE_BIN", example("fake_drive"))
            .env("IGN1T10N_GAZE_BIN", self.root.join("f1r3gaze"))
            .env("IGN1T10N_DEV_SECRETS", "1")
            .env("IGN1T10N_QUIET", "1")
            .env("IGN1T10N_SKIP_RESOURCE_CHECK", "1")
            .env("IGN1T10N_START_TIMEOUT_SECS", "45")
            .env("IGN1T10N_FAKE_MOUNTS", "1")
            .output()
            .unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    fn status(&self) -> serde_json::Value {
        let (ok, out) = self.cmd(&["ctl", "status", "--json"]);
        assert!(ok, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    fn manifest(&self) -> ign1t10n::manifest::Manifest {
        let path = self.root.join("state/shard.toml");
        let t = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        toml::from_str(&t).unwrap()
    }

    /// Spawn `ign1t10n supervise` detached, as launchd would.
    fn spawn_supervisor(&self) {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(exe());
        cmd.arg("supervise")
            .env("HOME", &self.home)
            .env("IGN1T10N_STATE_DIR", self.root.join("state"))
            .env("IGN1T10N_LOG_DIR", self.root.join("logs"))
            .env("F1R3GAZE_PROFILE", self.root.join("profile"))
            .env("IGN1T10N_NODE_BIN", example("fake_node"))
            .env("IGN1T10N_EMBERS_BIN", example("fake_node"))
            .env("IGN1T10N_SIDECAR_BIN", example("fake_sidecar"))
            .env("IGN1T10N_F1R3DRIVE_BIN", example("fake_drive"))
            .env("IGN1T10N_GAZE_BIN", self.root.join("f1r3gaze"))
            .env("IGN1T10N_DEV_SECRETS", "1")
            .env("IGN1T10N_SKIP_RESOURCE_CHECK", "1")
            .env("IGN1T10N_START_TIMEOUT_SECS", "45")
            .env("IGN1T10N_FAKE_MOUNTS", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().unwrap();
    }

    fn wait<F: Fn(&serde_json::Value) -> bool>(&self, what: &str, secs: u64, f: F) {
        let t0 = Instant::now();
        loop {
            let s = self.status();
            if f(&s) {
                return;
            }
            if t0.elapsed() > Duration::from_secs(secs) {
                panic!("timed out waiting for {what}: {s:#}");
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

fn sidecar_alive(port: u16) -> bool {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    let Ok(mut s) = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)) else {
        return false;
    };
    let _ = s.write_all(b"GET /files/__smoke__ HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n");
    let mut first = String::new();
    BufReader::new(&s).read_line(&mut first).is_ok() && first.starts_with("HTTP/1.1 ")
}

#[test]
fn sidecars_run_and_drive_round_trips() {
    let e = Env::new();
    let (ok, out) = e.cmd(&["ctl", "provision", "--non-interactive"]);
    assert!(ok, "provision failed: {out}");
    e.wait("running", 90, |s| s["shard"]["state"] == "running");

    // Four shard nodes (bootstrap + 2 validators + observer) each paired with
    // a file-read-service sidecar listed in the manifest.
    let m = e.manifest();
    assert_eq!(m.sidecar_configs.len(), 4, "one sidecar per shard node");
    for (_name, sc) in &m.sidecar_configs {
        assert!(sidecar_alive(sc.port), "sidecar on {} is not answering", sc.port);
    }

    // `ctl drive add` writes a drive entry with its own deployer key.
    let (ok, out) = e.cmd(&["ctl", "drive", "add", "alice"]);
    assert!(ok, "drive add: {out}");
    let m = e.manifest();
    assert!(m.drives.contains_key("alice"));
    let d = &m.drives["alice"];
    assert!(std::path::Path::new(&d.key_file).exists(), "deployer key written");

    // `ctl drive up` spawns the daemon and waits for the mount marker.
    let (ok, out) = e.cmd(&["ctl", "drive", "up", "alice"]);
    assert!(ok, "drive up: {out}");
    let d_up = &e.manifest().drives["alice"].clone();
    assert!(d_up.pid.is_some(), "pid recorded while running");
    let (ok, out) = e.cmd(&["ctl", "drive", "status"]);
    assert!(ok && out.contains("alice"), "{out}");
    assert!(out.contains("running"), "status reports running: {out}");

    // Read + write through the mount, matching the real-FUSE flow.
    let mount: PathBuf = d_up.mount_dir.clone().into();
    let greeting = mount.join("greeting.txt");
    std::fs::write(&greeting, b"hi").unwrap();
    assert_eq!(std::fs::read(&greeting).unwrap(), b"hi");

    // `ctl drive down` stops the daemon; the marker is cleared.
    let (ok, out) = e.cmd(&["ctl", "drive", "down", "alice"]);
    assert!(ok, "drive down: {out}");
    assert!(e.manifest().drives["alice"].pid.is_none(), "pid cleared");
    assert!(!mount.join(".f1r3drive-fake-mount").exists(), "mount marker cleared");

    // `ctl drive remove` removes the state dir and the manifest entry.
    let (ok, out) = e.cmd(&["ctl", "drive", "remove", "alice"]);
    assert!(ok, "drive remove: {out}");
    assert!(!e.manifest().drives.contains_key("alice"));
    assert!(!e.root.join("state/drives/alice").exists(), "state dir gone");
}

#[test]
fn drive_repoint_switches_wiring_and_cycles_mount() {
    let e = Env::new();
    assert!(e.cmd(&["ctl", "provision", "--non-interactive"]).0);
    e.wait("running", 90, |s| s["shard"]["state"] == "running");
    assert!(e.cmd(&["ctl", "drive", "add", "carol"]).0);
    assert!(e.cmd(&["ctl", "drive", "up", "carol"]).0);
    let before = e.manifest().drives["carol"].clone();

    // Rejects an unknown target without touching the manifest.
    let (ok, out) = e.cmd(&["ctl", "drive", "repoint", "carol", "--validator", "validator-99"]);
    assert!(!ok && out.contains("unknown validator"), "{out}");
    assert_eq!(e.manifest().drives["carol"].validator, before.validator);

    // Rejects a no-op call.
    let (ok, out) = e.cmd(&["ctl", "drive", "repoint", "carol"]);
    assert!(!ok && out.contains("--validator"), "{out}");

    // Pick a different validator + its sidecar; the drive should cycle.
    let other = if before.validator == "validator-1" { "validator-2" } else { "validator-1" };
    let (ok, out) = e.cmd(&["ctl", "drive", "repoint", "carol", "--validator", other, "--sidecar", other]);
    assert!(ok, "repoint: {out}");
    let after = e.manifest().drives["carol"].clone();
    assert_eq!(after.validator, other);
    assert_eq!(after.sidecar, other);
    // Still mounted, with a fresh pid.
    assert!(after.pid.is_some(), "drive stayed up after repoint");
    assert_ne!(after.pid, before.pid, "pid changed after cycle");
}

#[test]
fn drives_auto_remount_after_supervisor_restart() {
    let e = Env::new();
    let (ok, out) = e.cmd(&["ctl", "provision", "--non-interactive"]);
    assert!(ok, "provision failed: {out}");
    e.wait("running", 90, |s| s["shard"]["state"] == "running");
    assert!(e.cmd(&["ctl", "drive", "add", "bob"]).0);
    assert!(e.cmd(&["ctl", "drive", "up", "bob"]).0);
    let mount: PathBuf = e.manifest().drives["bob"].mount_dir.clone().into();
    assert!(mount.join(".f1r3drive-fake-mount").exists());

    // Supervisor exits as launchd does on logout: SIGTERM. The supervisor
    // runs its ordered stop (nodes + sidecars + drive daemons), but
    // `stop_nodes` does not call `drive::down`, so the drive's pid stays
    // set in the manifest — proof the user wanted this drive mounted.
    let pid: u32 = std::fs::read_to_string(e.root.join("state/run/supervisor.pid")).unwrap().trim().parse().unwrap();
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    for _ in 0..300 {
        if !e.root.join("state/run/supervisor.pid").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!e.root.join("state/run/supervisor.pid").exists(), "supervisor did not exit cleanly");
    assert!(e.manifest().drives["bob"].pid.is_some(), "pid preserved across clean shutdown");

    // Fresh supervisor comes up; the drive is remounted automatically.
    e.spawn_supervisor();
    // Wait for the socket to answer, then wait for `running`.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(60) {
        if e.cmd(&["ctl", "status", "--json"]).0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    e.wait("running after restart", 90, |s| s["shard"]["state"] == "running");
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(30) {
        if mount.join(".f1r3drive-fake-mount").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(mount.join(".f1r3drive-fake-mount").exists(), "drive bob was not auto-remounted");

    // Cleanly down → pid clears → next restart leaves drive down.
    assert!(e.cmd(&["ctl", "drive", "down", "bob"]).0);
    assert!(e.manifest().drives["bob"].pid.is_none());
}
