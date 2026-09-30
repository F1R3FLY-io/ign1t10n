//! Administrative deploys: faucet transfers, bonds, withdrawals and sweeps,
//! signed inside ign1t10n with `gaze_shard::deploy::sign` and sent to a
//! randomly chosen active validator other than the one the deploy concerns
//! (Decision 5; a bond must reach a bonded validator, never the joiner).

use crate::amounts::{ADMIN_PHLO_LIMIT, PHLO_PRICE};
use crate::api::Api;
use crate::keys::Key;
use gaze_shard::deploy::{DeployData, sign};
use std::time::{Duration, Instant};

pub struct Sent {
    pub id: String,
    pub target: String,
}

pub fn submit(target: &Api, key: &Key, term: &str, shard_id: &str) -> Result<Sent, String> {
    let (_, lfb) = target.last_finalized()?;
    let now = crate::now_ms();
    let d = sign(
        &key.signing,
        DeployData {
            term: term.into(),
            timestamp: now,
            phlo_price: PHLO_PRICE,
            phlo_limit: ADMIN_PHLO_LIMIT,
            valid_after_block_number: lfb,
            shard_id: shard_id.into(),
            expiration_timestamp: Some(now + 10 * 60 * 1000),
        },
    )?;
    target.deploy(&d)?;
    Ok(Sent { id: d.id(), target: target.base.clone() })
}

/// Wait until the deploy is finalised; `Err` if it failed or expired.
pub fn await_finalized(target: &Api, id: &str, timeout: Duration) -> Result<(), String> {
    let t0 = Instant::now();
    loop {
        match target.finalization(id) {
            Ok((s, _)) if s == "Finalized" => return Ok(()),
            Ok((s, _)) if s == "Failed" || s == "Expired" => return Err(format!("deploy {}… {s}", &id[..id.len().min(12)])),
            _ => {}
        }
        if t0.elapsed() > timeout {
            return Err(format!("deploy {}… not finalised after {}s", &id[..id.len().min(12)], timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Poll `f` until it returns `Some`, or time out.
pub fn wait_for<T>(what: &str, timeout: Duration, every: Duration, mut f: impl FnMut() -> Option<T>) -> Result<T, String> {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return Ok(v);
        }
        if t0.elapsed() > timeout {
            return Err(format!("timed out after {}s waiting for {what}", timeout.as_secs()));
        }
        std::thread::sleep(every);
    }
}
