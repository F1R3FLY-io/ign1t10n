//! Node processes: argument vectors (spec §7.1), a reduced environment, the
//! PEM password only in the process's own environment, output to a rotating
//! log, and ordered termination.

use crate::logging::{MB, Rotating};
use crate::manifest::Manifest;
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
    /// The F1R3Games portal (`f1r3games-service serve`), spec v0.4 §10.
    Portal,
}

impl Role {
    pub fn name(&self) -> String {
        match self {
            Role::Bootstrap => "bootstrap".into(),
            Role::Validator(k) => crate::nodeconf::validator_name(*k),
            Role::Observer => "observer".into(),
            Role::Embers => "embers".into(),
            Role::Portal => "portal".into(),
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "bootstrap" => Some(Role::Bootstrap),
            "observer" => Some(Role::Observer),
            "embers" => Some(Role::Embers),
            "portal" => Some(Role::Portal),
            _ => s.strip_prefix("validator-").and_then(|k| k.parse().ok()).map(Role::Validator),
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
        Role::Embers | Role::Portal => return Err(format!("{} is not a node", role.name())),
    }
    // Ports on the command line: the node's port options have default values
    // (40400, 40403, ...), and a default counts as given, so it overrides the
    // port in the configuration file. Every node would start on the defaults.
    let base = match role {
        Role::Bootstrap => m.bootstrap.base_port,
        Role::Observer => m.observer.base_port,
        Role::Validator(k) => m.validator(*k).ok_or_else(|| format!("no validator {k}"))?.base_port,
        Role::Embers | Role::Portal => unreachable!(),
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
    }
}
