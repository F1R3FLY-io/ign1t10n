//! A stand-in for `f1r3node` (and for `embers`) that serves the parts of the
//! HTTP API ign1t10n uses, so the supervisor, provisioning, genesis, restarts
//! and resizing can be tested end to end without a real shard. It is not a
//! blockchain: every deploy "finalises", balances grow with time, and a key
//! is bonded once its bond deploy has been seen.
//!
//! For F1R3Games it keeps just enough of the registry for the real
//! `f1r3games-service` jobs to install and register against it: a deploy
//! carrying `insertSigned` registers the first `rho:id:` URI in its term at
//! the version it names, the `env.probe` read answers that version, and
//! `games.register` stores the manifest (parsed from the term's Rholang
//! literal) that `games.get` returns. A deploy calling F1R3Ink's `setRelay`
//! is recorded in `nodes/fake-relay.txt`, one line per naming.

use std::collections::{BTreeMap, BTreeSet};
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

// ---- a little Rholang: the literals `Value::render` writes ----

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && (self.s[self.i] as char).is_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(t.as_bytes()) {
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Option<String> {
        self.ws();
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\\' => {
                    out.push(*self.s.get(self.i + 1)?);
                    self.i += 2;
                }
                b'"' => {
                    self.i += 1;
                    return String::from_utf8(out).ok();
                }
                c => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        None
    }
    fn seq(&mut self, close: &str) -> Option<Vec<serde_json::Value>> {
        let mut v = Vec::new();
        if self.eat(close) {
            return Some(v);
        }
        loop {
            v.push(self.value()?);
            if self.eat(close) {
                return Some(v);
            }
            if !self.eat(",") {
                return None;
            }
            if self.eat(close) {
                return Some(v); // a one-tuple's trailing comma
            }
        }
    }
    /// A value as the node's JSON expression (`ExprString`, `ExprMap`, ...).
    fn value(&mut self) -> Option<serde_json::Value> {
        use serde_json::json;
        self.ws();
        if self.eat("Nil") {
            return Some(json!({"ExprPar": {"data": []}}));
        }
        if self.eat("true") {
            return Some(json!({"ExprBool": {"data": true}}));
        }
        if self.eat("false") {
            return Some(json!({"ExprBool": {"data": false}}));
        }
        if self.eat("[") {
            return Some(json!({"ExprList": {"data": self.seq("]")?}}));
        }
        if self.eat("(") {
            return Some(json!({"ExprTuple": {"data": self.seq(")")?}}));
        }
        if self.eat("{") {
            let mut m = serde_json::Map::new();
            if self.eat("}") {
                return Some(json!({"ExprMap": {"data": m}}));
            }
            loop {
                let k = self.string()?;
                if !self.eat(":") {
                    return None;
                }
                m.insert(k, self.value()?);
                if self.eat("}") {
                    return Some(json!({"ExprMap": {"data": m}}));
                }
                if !self.eat(",") {
                    return None;
                }
            }
        }
        if self.eat("`") {
            let start = self.i;
            while self.i < self.s.len() && self.s[self.i] != b'`' {
                self.i += 1;
            }
            let u = String::from_utf8(self.s[start..self.i].to_vec()).ok()?;
            self.i += 1;
            return Some(json!({"ExprUri": {"data": u}}));
        }
        if let Some(st) = self.string() {
            if self.eat(".hexToBytes()") {
                return Some(json!({"ExprBytes": {"data": st}}));
            }
            return Some(json!({"ExprString": {"data": st}}));
        }
        let start = self.i;
        if self.s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        let n: i64 = std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()?;
        Some(json!({"ExprInt": {"data": n}}))
    }
}

fn first_uri(term: &str) -> Option<String> {
    let i = term.find("rho:id:")?;
    Some(term[i..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == ':').collect())
}

fn quoted_after(term: &str, marker: &str) -> Option<String> {
    let i = term.find(marker)? + marker.len();
    Some(term[i..].chars().take_while(|c| *c != '"').collect())
}

/// The F1R3Games part of the fake chain: environments by URI, registry by game.
fn games_deploy(dir: &std::path::Path, term: &str) {
    if term.contains("insertSigned") {
        if let (Some(uri), Some(v)) = (first_uri(term), term.find("if (version < ").and_then(|i| term[i + 14..].split(')').next()?.trim().parse::<i64>().ok())) {
            let mut envs: BTreeMap<String, i64> = std::fs::read_to_string(dir.join("nodes/fake-envs.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            envs.insert(uri, v);
            let _ = std::fs::write(dir.join("nodes/fake-envs.json"), serde_json::to_string(&envs).unwrap());
        }
    }
    if term.contains("@env!(\"setRelay\", ") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("nodes/fake-relay.txt")) {
            let _ = writeln!(f, "setRelay");
        }
    }
    let marker = "@env!(\"games\", \"register\", ";
    if let Some(i) = term.find(marker) {
        let mut p = P { s: term[i + marker.len()..].as_bytes(), i: 0 };
        if let Some(mut m) = p.value() {
            let id = m["ExprMap"]["data"]["id"]["ExprString"]["data"].as_str().unwrap_or("").to_string();
            m["ExprMap"]["data"]["status"] = serde_json::json!({"ExprString": {"data": "active"}});
            let _ = std::fs::create_dir_all(dir.join("nodes/fake-registry"));
            let _ = std::fs::write(dir.join("nodes/fake-registry").join(format!("{id}.json")), m.to_string());
        }
    }
}

/// The F1R3Games reads, or None for any other term.
fn games_explore(dir: &std::path::Path, term: &str) -> Option<serde_json::Value> {
    use serde_json::json;
    let nil = json!({"ExprPar": {"data": []}});
    if let Some(id) = quoted_after(term, "\"games\", \"get\", \"") {
        let m = std::fs::read_to_string(dir.join("nodes/fake-registry").join(format!("{id}.json"))).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(nil);
        return Some(json!({"ExprTuple": {"data": [{"ExprBool": {"data": true}}, m]}}));
    }
    if term.contains("ret!(version)") {
        let envs: BTreeMap<String, i64> = std::fs::read_to_string(dir.join("nodes/fake-envs.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        return Some(first_uri(term).and_then(|u| envs.get(&u).copied()).map(|v| json!({"ExprInt": {"data": v}})).unwrap_or(nil));
    }
    if term.contains("@env!(") {
        return Some(json!({"ExprTuple": {"data": [{"ExprBool": {"data": false}}, {"ExprString": {"data": "not in the fake node"}}]}}));
    }
    None
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
        let chain_dir4 = chain_dir.clone();
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
                "/api/blocks/1" => (200, format!(r#"[{{"blockInfo":{{"blockHash":"b{lfb}","blockNumber":{lfb}}}}}]"#)),
                "/api/deploy" => {
                    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let term = v["data"]["term"].as_str().unwrap_or("");
                    let who = v["deployer"].as_str().unwrap_or("").to_string();
                    if term.contains("\"bond\"") { record(format!("bond {who}")); }
                    if term.contains("\"withdraw\"") { record(format!("withdraw {who}")); }
                    if let Some(d) = &chain_dir4 { games_deploy(d, term); }
                    // As the node answers: the deploy id is the signature.
                    let sig = v["signature"].as_str().unwrap_or("");
                    (200, serde_json::Value::String(format!("Success!\nDeployId is: {sig}")).to_string())
                }
                p if p.starts_with("/api/deploy-finalization-status/") => (200, r#"{"state":"Finalized"}"#.into()),
                p if p.starts_with("/api/bond-status/") => {
                    let k = p.rsplit('/').next().unwrap().to_string();
                    (200, format!(r#"{{"isBonded":{}}}"#, active().contains(&k)))
                }
                p if p.starts_with("/api/balance/") => (200, format!(r#"{{"balance":{}}}"#, 1_000_000_000_000i64 + t0.elapsed().as_millis() as i64 * 1_000)),
                "/api/explore-deploy" => {
                    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let term = v["term"].as_str().unwrap_or("");
                    match chain_dir4.as_ref().and_then(|d| games_explore(d, term)) {
                        Some(e) => (200, serde_json::json!({ "expr": [e], "block": { "blockHash": format!("b{lfb}"), "blockNumber": lfb } }).to_string()),
                        None => (200, serde_json::json!({ "expr": [{ "ExprList": active().into_iter().map(|k| serde_json::json!({ "ExprBytes": { "data": k } })).collect::<Vec<_>>() }] }).to_string()),
                    }
                }
                _ => (404, r#"{"error":"endpoint_not_found"}"#.into()),
            };
            let mut w = s;
            let _ = write!(w, "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len());
        });
    }
}
