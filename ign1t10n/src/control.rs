//! The control socket (spec §5.2): `run/control.sock`, a Unix-domain socket in
//! a 0700 directory, one JSON object per line each way. The supervisor
//! refuses any peer whose user id is not its own.

use crate::paths::Paths;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    Status,
    Start,
    Stop,
    Restart,
    /// Change the number of validators (2..=10).
    Resize { target: u8, new_shard: bool },
    Retry,
    Undo,
    /// Faucet transfer to a F1R3Gaze wallet, in whole F1R3.
    Fund { address: String, f1r3: i64 },
    /// Installation options that can change later.
    SetOptions { embers: Option<bool>, gaze_integration: Option<bool> },
    /// Move a node whose port is taken to a free block.
    Reallocate { node: String },
    /// F1R3Games settings (spec v0.4 §11.2): on/off, the breeder, the
    /// faucet amount for new portal keys, opening the browser at first run.
    GamesSet {
        #[serde(default)]
        on: Option<bool>,
        #[serde(default)]
        breeder: Option<bool>,
        #[serde(default)]
        faucet_f1r3: Option<i64>,
        #[serde(default)]
        open_at_first_run: Option<bool>,
    },
    /// Re-run G3..G6 (nothing already on the chain is deployed again).
    GamesReinstall,
    /// Apply an update that discards on-chain state (consent given).
    GamesUpdate,
    /// Move F1R3Games to a new decade of ports: new origins, so browser
    /// keystores at the old address do not follow (consent given).
    GamesMove,
    /// Stop the shard and exit the supervisor (uninstall, reset).
    Shutdown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Report>,
}

impl Response {
    pub fn ok(msg: impl Into<String>) -> Response {
        Response { ok: true, message: Some(msg.into()), ..Default::default() }
    }
    pub fn err(e: impl Into<String>) -> Response {
        Response { ok: false, error: Some(e.into()), ..Default::default() }
    }
}

/// The shard's state machine (spec §7.2).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum ShardState {
    #[default]
    Stopped,
    Starting(String),
    Running,
    Degraded(String),
    Resizing(String),
    Failed(String),
}

impl ShardState {
    pub fn label(&self) -> String {
        match self {
            ShardState::Stopped => "Stopped".into(),
            ShardState::Starting(s) => format!("Starting ({s})"),
            ShardState::Running => "Running".into(),
            ShardState::Degraded(r) => format!("Degraded: {r}"),
            ShardState::Resizing(s) => format!("Resizing: {s}"),
            ShardState::Failed(r) => format!("Failed: {r}"),
        }
    }
}

/// F1R3Games' own state machine (spec v0.4 §8.2), beside the shard's.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum GamesState {
    #[default]
    Off,
    /// The shard is not running.
    Waiting,
    Installing(String),
    Running,
    Degraded(String),
    Failed(String),
}

impl GamesState {
    pub fn label(&self) -> String {
        match self {
            GamesState::Off => "Off".into(),
            GamesState::Waiting => "Waiting for the shard".into(),
            GamesState::Installing(s) => format!("Installing ({s})"),
            GamesState::Running => "Running".into(),
            GamesState::Degraded(r) => format!("Degraded: {r}"),
            GamesState::Failed(r) => format!("Failed: {r}"),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct GameReport {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub env_version: i64,
    pub client: bool,
    pub registered: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct GamesReport {
    pub state: GamesState,
    /// `http://localhost:<port>`.
    pub url: String,
    pub games: Vec<GameReport>,
    pub faucet_f1r3: i64,
    pub breeder: bool,
    pub open_at_first_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_update: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_epoch: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NodeReport {
    pub name: String,
    pub base_port: u16,
    /// Slot state for validators ("active", "joining", ...), role otherwise.
    pub state: String,
    pub running: bool,
    pub ready: bool,
    pub lfb: i64,
    pub peers: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub shard: ShardState,
    pub validators: u8,
    pub nodes: Vec<NodeReport>,
    pub shard_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_hash: Option<String>,
    /// Where F1R3Gaze currently sends deploys.
    pub gaze_validators: Vec<String>,
    pub observer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funded_wallet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize: Option<String>,
    /// Sockets found listening off loopback (empty when all is well).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed: Vec<String>,
    pub embers_enabled: bool,
    pub gaze_integration: bool,
    /// F1R3Games, when installed or chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub games: Option<GamesReport>,
}

pub fn call(paths: &Paths, req: &Request, timeout: Duration) -> Result<Response, String> {
    let mut s = UnixStream::connect(paths.socket()).map_err(|e| format!("the shard supervisor is not running ({e})"))?;
    s.set_read_timeout(Some(timeout)).ok();
    s.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let mut line = serde_json::to_string(req).unwrap();
    line.push('\n');
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s);
    let mut out = String::new();
    r.read_line(&mut out).map_err(|e| e.to_string())?;
    serde_json::from_str(&out).map_err(|e| format!("bad answer from the supervisor: {e}"))
}

/// The user id of the process at the other end of `s`.
pub fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let fd = s.as_raw_fd();
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    unsafe {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        if libc::getpeereid(fd, &mut uid, &mut gid) == 0 {
            return Some(uid);
        }
        None
    }
    #[cfg(target_os = "linux")]
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut libc::c_void, &mut len) == 0 {
            return Some(cred.uid);
        }
        None
    }
}

pub fn my_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_is_line_json() {
        let r = Request::Resize { target: 5, new_shard: false };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"op":"resize","target":5,"new_shard":false}"#);
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
        let st = serde_json::to_string(&ShardState::Degraded("observer lag".into())).unwrap();
        assert_eq!(st, r#"{"state":"degraded","detail":"observer lag"}"#);
    }

    #[test]
    fn peer_uid_is_ours() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a), Some(my_uid()));
    }
}
