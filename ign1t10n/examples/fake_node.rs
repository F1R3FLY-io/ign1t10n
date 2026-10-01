//! A stand-in for `f1r3node` (and for `embers`) that serves the parts of the
//! HTTP API ign1t10n uses, so the supervisor, provisioning, genesis, restarts
//! and resizing can be tested end to end without a real shard. It is not a
//! blockchain: every deploy "finalises", balances grow with time, and a key
//! is bonded once its bond deploy has been seen.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn conf_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim();
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Fake nodes share "chain state" through a file beside the state
    // directory's conf/: genesis bonds, then bond/withdraw lines.
    let chain_dir = args.windows(2).find(|w| w[0] == "--config-file").and_then(|w| std::path::Path::new(&w[1]).parent().and_then(|c| c.parent()).map(|s| s.to_path_buf()));
    let (port, id, role) = if let Ok(p) = std::env::var("EMBERS__PORT") {
        (p.parse::<u16>().unwrap(), "embers".to_string(), "embers".to_string())
    } else {
        let conf = args.windows(2).find(|w| w[0] == "--config-file").map(|w| w[1].clone()).expect("--config-file");
        let text = std::fs::read_to_string(&conf).unwrap();
        // As the real node: a port option's default overrides the config file.
        let _ = conf_value(&text, "port-http");
        let port: u16 = args.windows(2).find(|w| w[0] == "--api-port-http").map(|w| w[1].parse().unwrap()).unwrap_or(40403);
        let name = std::path::Path::new(&conf).file_stem().unwrap().to_string_lossy().to_string();
        let id = format!("{:040x}", name.bytes().fold(0u128, |a, b| a.wrapping_mul(131).wrapping_add(b as u128)));
        if args.iter().any(|a| a.contains("private-key-path")) && std::env::var("F1R3NODE_VALIDATOR_PASSWORD").is_err() {
            eprintln!("no password");
            std::process::exit(3);
        }
        if args.iter().any(|a| a == "--validator-private-key") {
            eprintln!("a key on the command line");
            std::process::exit(4);
        }
        (port, id, name)
    };
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| {
        eprintln!("bind {port}: {e}");
        std::process::exit(2)
    });
    eprintln!("fake {role} on {port}");
    // As the real node does at startup.
    println!(r#"{{"message":"Local peer node: rnode://{id}@127.0.0.1?protocol={}&discovery={}","target":"node"}}"#, port.saturating_sub(3), port + 1);
    let t0 = Instant::now();
    let lock: Arc<Mutex<()>> = Arc::default();
    for s in l.incoming().flatten() {
        let (id, role, chain_dir, lock) = (id.clone(), role.clone(), chain_dir.clone(), lock.clone());
        let chain_dir2 = chain_dir.clone();
        let chain_dir_s = chain_dir.clone().unwrap_or_default();
        let chain_dir3 = chain_dir.clone();
        let active = move || -> BTreeSet<String> {
            let Some(d) = &chain_dir else { return BTreeSet::new() };
            let mut set: BTreeSet<String> = std::fs::read_to_string(d.join("genesis/bonds.txt")).unwrap_or_default().lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect();
            for l in std::fs::read_to_string(d.join("fake-chain.txt")).unwrap_or_default().lines() {
                match l.split_once(' ') {
                    Some(("bond", k)) => { set.insert(k.to_string()); }
                    Some(("withdraw", k)) => { set.remove(k); }
                    _ => {}
                }
            }
            set
        };
        let record = { let chain_dir = chain_dir2; move |line: String| {
            let _g = lock.lock().unwrap();
            if let Some(d) = &chain_dir {
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(d.join("fake-chain.txt")).unwrap();
                let _ = writeln!(f, "{line}");
            }
        }};
        std::thread::spawn(move || {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            if r.read_line(&mut first).is_err() {
                return;
            }
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            let _ = r.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).to_string();
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
            eprintln!("{role}: {}", first.trim());
            let lfb = 1 + t0.elapsed().as_secs() as i64;
            let ready = t0.elapsed().as_millis() > 800;
            let (code, out) = match path.as_str() {
                "/api/service/ready" => (200, "{}".to_string()),
                // A real ceremony master has no Casper engine before genesis.
                "/api/status" if role == "bootstrap" && !std::path::Path::new(&chain_dir_s).join("genesis-done").exists() => (503, r#"{"error":"casper_not_ready"}"#.into()),
                "/api/status" => (200, format!(r#"{{"address":"rnode://{id}@127.0.0.1?protocol=1&discovery=2","isReady":{ready},"peers":3,"lastFinalizedBlockNumber":{lfb},"version":{{"node":"fake 0.0"}},"isValidator":{},"isReadOnly":{}}}"#, role.starts_with("validator"), role == "observer")),
                "/api/ready" => if ready { (200, r#"{"ready":true}"#.into()) } else { (503, r#"{"ready":false}"#.into()) },
                "/api/blocks/0/0" => {
                    if let Some(d) = &chain_dir3 { let _ = std::fs::write(d.join("genesis-done"), ""); }
                    (200, r#"[{"blockHash":"9e3a5fake0genesis","blockNumber":0}]"#.into())
                }
                "/api/blocks/0/0-unused" => (200, r#"[{"blockHash":"9e3a5fake0genesis","blockNumber":0}]"#.into()),
                "/api/last-finalized-block" => (200, format!(r#"{{"blockInfo":{{"blockHash":"b{lfb}","blockNumber":{lfb}}}}}"#)),
                "/api/deploy" => {
                    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let term = v["data"]["term"].as_str().unwrap_or("");
                    let who = v["deployer"].as_str().unwrap_or("").to_string();
                    if term.contains("\"bond\"") { record(format!("bond {who}")); }
                    if term.contains("\"withdraw\"") { record(format!("withdraw {who}")); }
                    (200, r#""Success!""#.into())
                }
                p if p.starts_with("/api/deploy-finalization-status/") => (200, r#"{"state":"Finalized"}"#.into()),
                p if p.starts_with("/api/bond-status/") => {
                    let k = p.rsplit('/').next().unwrap().to_string();
                    (200, format!(r#"{{"isBonded":{}}}"#, active().contains(&k)))
                }
                p if p.starts_with("/api/balance/") => (200, format!(r#"{{"balance":{}}}"#, 1_000_000_000_000i64 + t0.elapsed().as_millis() as i64 * 1_000)),
                "/api/explore-deploy" => (200, serde_json::json!({ "expr": [{ "ExprList": active().into_iter().map(|k| serde_json::json!({ "ExprBytes": { "data": k } })).collect::<Vec<_>>() }] }).to_string()),
                _ => (404, r#"{"error":"endpoint_not_found"}"#.into()),
            };
            let mut w = s;
            let _ = write!(w, "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len());
        });
    }
}
