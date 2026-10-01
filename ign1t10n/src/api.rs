//! A client for the parts of a node's HTTP API ign1t10n needs. Deploy
//! submission and finalisation go through F1R3Gaze's `gaze_shard::node::Node`
//! so that what ign1t10n sends is exactly what the browser sends.

use gaze_net::{Http, HttpRequest};
use gaze_shard::deploy::SignedDeploy;
use gaze_shard::node::Node;
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

/// Every node ign1t10n talks to is on loopback: a request that has not been
/// answered in a few seconds will not be, and must not stall the supervisor.
fn local_agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout(Duration::from_secs(10)).redirects(0).build()
}

#[derive(Clone)]
pub struct Api {
    pub base: String,
    /// F1R3Gaze's client, for deploys (signing and submission as F1R3Gaze does).
    http: Http,
    local: ureq::Agent,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub address: String,
    pub node_id: Option<String>,
    pub is_ready: bool,
    pub is_validator: bool,
    pub is_read_only: bool,
    pub peers: i64,
    pub lfb: i64,
    pub node_version: String,
    /// Work package N4.
    pub storage_format: Option<u32>,
}

/// `rnode://<id>@host?...` -> `<id>`
pub fn node_id_of(address: &str) -> Option<String> {
    let rest = address.strip_prefix("rnode://")?;
    let id = rest.split('@').next()?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| id.to_string())
}

impl Api {
    pub fn new(base: impl Into<String>) -> Api {
        Api { base: base.into().trim_end_matches('/').to_string(), http: Http::new(), local: local_agent() }
    }

    pub fn node(&self) -> Node {
        Node::new(&self.base, self.http.clone())
    }

    fn get(&self, path: &str) -> Result<(u16, Value), String> {
        let url = format!("{}{path}", self.base);
        let r = match self.local.get(&url).set("accept", "application/json").call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(format!("{url}: {e}")),
        };
        let status = r.status();
        let mut body = Vec::new();
        r.into_reader().take(16 << 20).read_to_end(&mut body).map_err(|e| format!("{url}: {e}"))?;
        Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }

    /// `GET path` answered 200, or why not.
    pub fn get_ok(&self, path: &str) -> Result<(), String> {
        match self.get(path)? {
            (200, _) => Ok(()),
            (s, v) => Err(format!("HTTP {s} {v}")),
        }
    }

    /// `GET /api/ready`, with the reason when it is not 200 (for the log).
    pub fn ready_why(&self) -> Result<(), String> {
        match self.get("/api/ready")? {
            (200, _) => Ok(()),
            (s, v) => Err(format!("HTTP {s} {v}")),
        }
    }

    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String> {
        let r = self
            .http
            .send(&HttpRequest {
                url: format!("{}{path}", self.base),
                method: "POST".into(),
                headers: vec![("content-type".into(), "application/json".into()), ("accept".into(), "application/json".into())],
                body: body.to_string().into_bytes(),
            })
            .map_err(|e| format!("{}{path}: {e}", self.base))?;
        Ok((r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null)))
    }

    fn ok(&self, path: &str, (s, v): (u16, Value)) -> Result<Value, String> {
        if (200..300).contains(&s) {
            Ok(v)
        } else {
            Err(format!("{}{path}: HTTP {s}: {}", self.base, v.get("message").map(|m| m.to_string()).unwrap_or_else(|| v.to_string())))
        }
    }

    pub fn status(&self) -> Result<Status, String> {
        let v = self.ok("/api/status", self.get("/api/status")?)?;
        let address = v["address"].as_str().unwrap_or("").to_string();
        Ok(Status {
            node_id: node_id_of(&address),
            address,
            is_ready: v["isReady"].as_bool().unwrap_or(false),
            is_validator: v["isValidator"].as_bool().unwrap_or(false),
            is_read_only: v["isReadOnly"].as_bool().unwrap_or(false),
            peers: v["peers"].as_i64().unwrap_or(0),
            lfb: v["lastFinalizedBlockNumber"].as_i64().unwrap_or(-1),
            node_version: v["version"]["node"].as_str().unwrap_or("").to_string(),
            storage_format: v["storageFormat"].as_u64().map(|x| x as u32),
        })
    }

    /// `GET /api/ready`: 200 once the node can serve deploys.
    pub fn ready(&self) -> bool {
        self.ready_why().is_ok()
    }

    pub fn bond_status(&self, public_key: &str) -> Result<bool, String> {
        let p = format!("/api/bond-status/{public_key}");
        let v = self.ok(&p, self.get(&p)?)?;
        Ok(v["isBonded"].as_bool().unwrap_or(false))
    }

    /// Vault balance in dust (observer only).
    pub fn balance(&self, address: &str) -> Result<i64, String> {
        let p = format!("/api/balance/{address}");
        let v = self.ok(&p, self.get(&p)?)?;
        v["balance"].as_i64().ok_or_else(|| format!("no balance in {v}"))
    }

    /// Exploratory deploy (observer only); the whole answer.
    pub fn explore(&self, term: &str) -> Result<Value, String> {
        self.ok("/api/explore-deploy", self.post("/api/explore-deploy", &json!({ "term": term }))?)
    }

    pub fn deploy(&self, d: &SignedDeploy) -> Result<String, String> {
        self.node().deploy(d)
    }

    /// `(state, block)`: Finalized, Failed, Pending or Expired.
    pub fn finalization(&self, id: &str) -> Result<(String, Option<String>), String> {
        self.node().finalization(id)
    }

    pub fn last_finalized(&self) -> Result<(String, i64), String> {
        self.node().last_finalized()
    }

    /// The genesis block's hash: block 0 of `GET /api/blocks/0/0`.
    pub fn genesis_hash(&self) -> Result<String, String> {
        let v = self.ok("/api/blocks/0/0", self.get("/api/blocks/0/0")?)?;
        find_key(&v, "blockHash").ok_or_else(|| format!("no blockHash in {v}"))
    }
}

/// The first string value under `key`, depth first.
pub fn find_key(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(m) => m.get(key).and_then(Value::as_str).map(str::to_string).or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn node_id_parsing() {
        assert_eq!(super::node_id_of("rnode://1e780e5d@127.0.0.1?protocol=40400&discovery=40404").as_deref(), Some("1e780e5d"));
        assert_eq!(super::node_id_of("http://x"), None);
    }
}
