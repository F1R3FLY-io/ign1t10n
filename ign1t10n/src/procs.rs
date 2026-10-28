//! Node processes: argument vectors (spec §7.1), a reduced environment, the
//! PEM password only in the process's own environment, output to a rotating
//! log, and ordered termination.

use crate::logging::{MB, Rotating};
use crate::manifest::{DriveConf, Manifest};
use crate::paths::Paths;
use crate::ports::Block;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Bootstrap,
    Validator(u8),
    Observer,
    Embers,
    /// A `file-read-service` HTTP sidecar paired with one shard node.
    FileReadService { node: String },
    /// An opt-in f1r3drive FUSE mount, keyed by its manifest alias.
    F1r3Drive { alias: String },
}

impl Role {
    pub fn name(&self) -> String {
        match self {
            Role::Bootstrap => "bootstrap".into(),
            Role::Validator(k) => crate::nodeconf::validator_name(*k),
            Role::Observer => "observer".into(),
            Role::Embers => "embers".into(),
            Role::FileReadService { node } => format!("sidecar-{node}"),
            Role::F1r3Drive { alias } => format!("drive-{alias}"),
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "bootstrap" => Some(Role::Bootstrap),
            "observer" => Some(Role::Observer),
            "embers" => Some(Role::Embers),
            _ => {
                if let Some(k) = s.strip_prefix("validator-").and_then(|k| k.parse().ok()) {
                    Some(Role::Validator(k))
                } else if let Some(node) = s.strip_prefix("sidecar-") {
                    Some(Role::FileReadService { node: node.into() })
                } else {
                    s.strip_prefix("drive-").map(|a| Role::F1r3Drive { alias: a.into() })
                }
            }
        }
    }
}

pub const TOKEN_FLAGS: [&str; 6] = ["--native-token-name", "F1R3CAP", "--native-token-symbol", "F1R3", "--native-token-decimals", "8"];

pub fn bootstrap_address(m: &Manifest) -> Option<String> {
    let b = Block(m.bootstrap.base_port);
    m.bootstrap.node_id.as_ref().map(|id| format!("rnode://{id}@127.0.0.1?protocol={}&discovery={}", b.protocol(), b.discovery()))
}

/// The argument vector for a node (not Embers).
pub fn node_args(p: &Paths, m: &Manifest, role: &Role) -> Result<Vec<String>, String> {
    let mut a: Vec<String> = vec!["run".into(), "--config-file".into(), p.conf_file(&role.name()).display().to_string()];
    let common = ["--host", "127.0.0.1", "--no-upnp", "--allow-private-addresses"];
    match role {
        Role::Bootstrap => {
            // The node's default `bootstrap` is empty: the bootstrap starts its
            // peer table alone, and ign1t10n reads its id from /api/status.
            a.extend(["--validator-private-key-path".into(), p.key("bootstrap").display().to_string()]);
            a.extend(["--required-signatures".into(), crate::nodeconf::required_signatures(m.shard.genesis_validators).to_string(), "--ceremony-master-mode".into(), "--heartbeat-disabled".into()]);
        }
        Role::Validator(k) => {
            a.extend(["--bootstrap".into(), bootstrap_address(m).ok_or("bootstrap node id unknown")?]);
            a.extend(["--validator-private-key-path".into(), p.key(&role.name()).display().to_string()]);
            let v = m.validator(*k).ok_or_else(|| format!("no validator {k}"))?;
            if v.genesis && m.shard.genesis_hash.is_none() {
                a.push("--genesis-validator".into());
            }
        }
        Role::Observer => {
            a.extend(["--bootstrap".into(), bootstrap_address(m).ok_or("bootstrap node id unknown")?]);
            a.push("--heartbeat-disabled".into());
        }
        Role::Embers | Role::FileReadService { .. } | Role::F1r3Drive { .. } => return Err("not a shard node".into()),
    }
    // Ports on the command line: the node's port options have default values
    // (40400, 40403, ...), and a default counts as given, so it overrides the
    // port in the configuration file. Every node would start on the defaults.
    let base = match role {
        Role::Bootstrap => m.bootstrap.base_port,
        Role::Observer => m.observer.base_port,
        Role::Validator(k) => m.validator(*k).ok_or_else(|| format!("no validator {k}"))?.base_port,
        Role::Embers | Role::FileReadService { .. } | Role::F1r3Drive { .. } => unreachable!(),
    };
    let b = crate::ports::Block(base);
    for (flag, port) in [
        ("--protocol-port", b.protocol()),
        ("--discovery-port", b.discovery()),
        ("--api-port-grpc-external", b.grpc_external()),
        ("--api-port-grpc-internal", b.grpc_internal()),
        ("--api-port-http", b.http()),
        ("--api-port-admin-http", b.admin()),
    ] {
        a.extend([flag.to_string(), port.to_string()]);
    }
    // Both the network id this node accepts and the one it sends (the CLI
    // option sets both; the configuration alone leaves the outgoing one at
    // its default, "testnet", and every peer refuses the messages).
    a.extend(["--network-id".to_string(), m.shard.network_id.clone()]);
    a.extend(common.iter().map(|s| s.to_string()));
    a.extend(TOKEN_FLAGS.iter().map(|s| s.to_string()));
    Ok(a)
}

/// Argument vector for a `file-read-service` sidecar: config file + bind.
pub fn sidecar_args(p: &Paths, node: &str, port: u16) -> Vec<String> {
    vec![
        "--config".into(),
        p.sidecar_conf(node).display().to_string(),
        "--bind".into(),
        format!("127.0.0.1:{port}"),
    ]
}

/// Argument vector for an `f1r3drive` daemon in `extension` mode. Panics on
/// the manifest being malformed is unreachable: `drive::up` validates its
/// `DriveConf` before spawn.
pub fn drive_args(_p: &Paths, m: &Manifest, d: &DriveConf) -> Result<Vec<String>, String> {
    let validator_base = m
        .validators
        .iter()
        .find(|v| crate::nodeconf::validator_name(v.slot) == d.validator)
        .map(|v| v.base_port)
        .ok_or_else(|| format!("drive points at unknown validator {}", d.validator))?;
    let observer_base = match d.observer.as_str() {
        "observer" => m.observer.base_port,
        "bootstrap" => m.bootstrap.base_port,
        other => m
            .validators
            .iter()
            .find(|v| crate::nodeconf::validator_name(v.slot) == other)
            .map(|v| v.base_port)
            .ok_or_else(|| format!("drive points at unknown observer {other}"))?,
    };
    let sidecar_port = m
        .sidecar_configs
        .get(&d.sidecar)
        .map(|s| s.port)
        .ok_or_else(|| format!("drive points at unknown sidecar {}", d.sidecar))?;
    let vb = Block(validator_base);
    let ob = Block(observer_base);
    let mut a: Vec<String> = vec![
        d.mount_dir.clone(),
        "--mount-mode".into(),
        "extension".into(),
        "--state-dir".into(),
        d.state_dir.clone(),
        "--key-file".into(),
        d.key_file.clone(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        vb.grpc_external().to_string(),
        "--observer-host".into(),
        "127.0.0.1".into(),
        "--observer-port".into(),
        ob.grpc_external().to_string(),
        "--sidecar-url".into(),
        format!("http://127.0.0.1:{sidecar_port}"),
        "--shard-id".into(),
        m.shard.id.clone(),
        "--chain-blobs".into(),
    ];
    for pk in &d.tracked_pubkeys {
        a.extend(["--track-pubkey".into(), pk.clone()]);
    }
    Ok(a)
}

/// The inherited environment: HOME, PATH, TMPDIR, LANG only.
pub fn base_env() -> Vec<(String, String)> {
    let mut e: Vec<(String, String)> = ["HOME", "PATH", "TMPDIR", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    e.push(("OPENAI_ENABLED".into(), "false".into()));
    e.push(("OLLAMA_ENABLED".into(), "false".into()));
    e
}

pub struct Proc {
    pub role: Role,
    pub child: Child,
    pub started: Instant,
}

pub fn spawn(program: &std::path::Path, args: &[String], env: &[(String, String)], log: std::path::PathBuf, role: Role) -> Result<Proc, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).env_clear().envs(env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Own process group, so a terminal's ^C to `ctl` never reaches a node.
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", program.display()))?;
    let sink = std::sync::Arc::new(std::sync::Mutex::new(Rotating::new(log, 10 * MB, 3)));
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        let sink = sink.clone();
        std::thread::spawn(move || {
            let mut s = stream;
            let mut buf = [0u8; 8192];
            while let Ok(n) = s.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut w) = sink.lock() {
                    w.write(&buf[..n]);
                }
            }
        });
    }
    Ok(Proc { role, child, started: Instant::now() })
}

pub fn signal(p: &Proc, sig: i32) {
    unsafe {
        libc::kill(p.child.id() as i32, sig);
    }
}

/// SIGTERM, wait up to `grace`, then SIGKILL. Returns whether it exited in time.
pub fn stop_all(procs: &mut [Proc], grace: Duration) -> bool {
    for p in procs.iter() {
        signal(p, libc::SIGTERM);
    }
    let t0 = Instant::now();
    loop {
        let alive = procs.iter_mut().filter_map(|p| p.child.try_wait().ok()).filter(Option::is_none).count();
        if alive == 0 {
            return true;
        }
        if t0.elapsed() > grace {
            for p in procs.iter_mut() {
                if matches!(p.child.try_wait(), Ok(None)) {
                    signal(p, libc::SIGKILL);
                    let _ = p.child.wait();
                }
            }
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_vectors_never_carry_keys() {
        let p = Paths::from_env();
        let mut m = crate::manifest::tests_support::sample();
        assert!(node_args(&p, &m, &Role::Validator(1)).is_err(), "no bootstrap id yet");
        m.bootstrap.node_id = Some("ab12".into());
        let v = node_args(&p, &m, &Role::Validator(1)).unwrap();
        assert!(v.contains(&"--genesis-validator".to_string()));
        assert!(v.iter().any(|a| a == "rnode://ab12@127.0.0.1?protocol=40400&discovery=40404"));
        assert!(!v.iter().any(|a| a == "--validator-private-key"));
        m.shard.genesis_hash = Some("h".into());
        assert!(!node_args(&p, &m, &Role::Validator(1)).unwrap().contains(&"--genesis-validator".to_string()));
        let b = node_args(&p, &m, &Role::Bootstrap).unwrap();
        assert!(!b.contains(&"--bootstrap".to_string()));
        assert!(b.windows(2).any(|w| w[0] == "--required-signatures" && w[1] == "1"), "N0 - 1 approvals");
        let v2 = node_args(&p, &m, &Role::Validator(2)).unwrap();
        assert!(v2.windows(2).any(|w| w[0] == "--protocol-port" && w[1] == "40420"), "ports on the command line");
        assert!(v2.windows(2).any(|w| w[0] == "--api-port-http" && w[1] == "40423"));
        assert!(v2.windows(2).any(|w| w[0] == "--network-id" && w[1] == m.shard.network_id));
        let o = node_args(&p, &m, &Role::Observer).unwrap();
        assert!(!o.iter().any(|a| a.contains("private-key")));
        assert_eq!(Role::parse("validator-7"), Some(Role::Validator(7)));
        assert_eq!(Role::parse("sidecar-validator-1"), Some(Role::FileReadService { node: "validator-1".into() }));
        assert_eq!(Role::parse("drive-alice"), Some(Role::F1r3Drive { alias: "alice".into() }));
    }

    #[test]
    fn sidecar_argv_names_the_config_and_bind() {
        let p = Paths::from_env();
        let a = sidecar_args(&p, "validator-1", 41410);
        assert!(a.windows(2).any(|w| w[0] == "--bind" && w[1] == "127.0.0.1:41410"));
        assert!(a.windows(2).any(|w| w[0] == "--config" && w[1].ends_with("sidecar-validator-1.conf")));
    }

    #[test]
    fn drive_argv_wires_validator_observer_sidecar() {
        let p = Paths::from_env();
        let mut m = crate::manifest::tests_support::sample();
        m.sidecar_configs.insert("validator-1".into(), crate::manifest::SidecarConf { port: 41410 });
        let d = DriveConf {
            state_dir: "/tmp/d/alice".into(),
            key_file: "/tmp/d/alice/deployer.hex".into(),
            mount_dir: "/tmp/f1r3drive/alice".into(),
            validator: "validator-1".into(),
            observer: "observer".into(),
            sidecar: "validator-1".into(),
            tracked_pubkeys: vec!["04deadbeef".into()],
            pid: None,
        };
        let a = drive_args(&p, &m, &d).unwrap();
        assert_eq!(a[0], d.mount_dir);
        assert!(a.windows(2).any(|w| w[0] == "--mount-mode" && w[1] == "extension"));
        assert!(a.windows(2).any(|w| w[0] == "--port" && w[1] == "40411"), "validator-1 grpc_external");
        assert!(a.windows(2).any(|w| w[0] == "--observer-port" && w[1] == "40451"));
        assert!(a.windows(2).any(|w| w[0] == "--sidecar-url" && w[1] == "http://127.0.0.1:41410"));
        assert!(a.iter().any(|x| x == "--chain-blobs"));
        assert!(a.windows(2).any(|w| w[0] == "--track-pubkey" && w[1] == "04deadbeef"));
    }
}
