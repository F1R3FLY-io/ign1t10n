//! Embers, bundled by default (Decision 8): the F1R3Sky wallet and agent
//! service, so that F1R3Gaze's wallet panel shows balances and history and can
//! send transfers against the local shard. It runs as one more supervised
//! process, configured through `EMBERS__*` environment variables (Embers'
//! `configuration.rs`), on loopback.
//!
//! Embers needs a funded service key per network and one key per registry
//! environment (wallets, agents, agent teams, OSLFs; testnet), plus an AES
//! key. All are generated here, kept together in one Keychain item
//! (`embers.secrets`), and reach Embers only through its environment. The
//! local shard serves as both Embers' "mainnet" and its "testnet".

use crate::keys::Key;
use crate::manifest::{Account, Manifest};
use crate::ports::Block;
use crate::secrets::Secrets;
use serde::{Deserialize, Serialize};

pub const ACCOUNT: &str = "embers.secrets";

#[derive(Clone, Serialize, Deserialize)]
pub struct EmbersSecrets {
    pub service_key: String,
    pub testnet_service_key: String,
    pub wallets_env_key: String,
    pub agents_env_key: String,
    pub agents_teams_env_key: String,
    pub oslfs_env_key: String,
    pub testnet_env_key: String,
    pub aes_encryption_key: String,
}

impl EmbersSecrets {
    pub fn generate() -> EmbersSecrets {
        let k = || Key::generate().secret_hex();
        EmbersSecrets {
            service_key: k(),
            testnet_service_key: k(),
            wallets_env_key: k(),
            agents_env_key: k(),
            agents_teams_env_key: k(),
            oslfs_env_key: k(),
            testnet_env_key: k(),
            aes_encryption_key: hex::encode(crate::random_bytes::<32>()),
        }
    }
}

pub fn load_or_create(s: &dyn Secrets) -> Result<EmbersSecrets, String> {
    if let Some(t) = s.get(ACCOUNT)? {
        return serde_json::from_str(&t).map_err(|e| format!("embers secrets: {e}"));
    }
    let e = EmbersSecrets::generate();
    s.set(ACCOUNT, &serde_json::to_string(&e).unwrap())?;
    Ok(e)
}

pub fn accounts(e: &EmbersSecrets) -> Result<(Account, Account), String> {
    let acct = |h: &str| -> Result<Account, String> {
        let k = Key::from_secret_hex(h)?;
        Ok(Account { public_key: k.public_hex(), address: k.address() })
    };
    Ok((acct(&e.service_key)?, acct(&e.testnet_service_key)?))
}

/// Embers' environment, pointed at validator `slot` (deploy and propose) and
/// the observer (reads, events).
pub fn env(m: &Manifest, e: &EmbersSecrets, slot: u8) -> Result<Vec<(String, String)>, String> {
    let em = m.embers.as_ref().ok_or("embers not configured")?;
    let v = Block(m.validator(slot).ok_or("no such validator")?.base_port);
    let o = Block(m.observer.base_port);
    let mut env = crate::procs::base_env();
    let mut put = |k: &str, v: String| env.push((format!("EMBERS__{k}"), v));
    put("ADDRESS", "127.0.0.1".into());
    put("PORT", em.port.to_string());
    put("LOG_LEVEL", "info".into());
    put("AES_ENCRYPTION_KEY", e.aes_encryption_key.clone());
    for net in ["MAINNET", "TESTNET"] {
        put(&format!("{net}__DEPLOY_SERVICE_URL"), format!("http://127.0.0.1:{}", v.grpc_external()));
        put(&format!("{net}__PROPOSE_SERVICE_URL"), format!("http://127.0.0.1:{}", v.grpc_internal()));
        put(&format!("{net}__VALIDATOR_WS_API_URL"), format!("ws://127.0.0.1:{}/ws/events", v.http()));
        put(&format!("{net}__OBSERVER_URL"), o.http_url());
        put(&format!("{net}__OBSERVER_WS_API_URL"), format!("ws://127.0.0.1:{}/ws/events", o.http()));
    }
    put("MAINNET__SERVICE_KEY", e.service_key.clone());
    put("MAINNET__WALLETS_ENV_KEY", e.wallets_env_key.clone());
    put("MAINNET__AGENTS_ENV_KEY", e.agents_env_key.clone());
    put("MAINNET__AGENTS_TEAMS_ENV_KEY", e.agents_teams_env_key.clone());
    put("MAINNET__OSLFS_ENV_KEY", e.oslfs_env_key.clone());
    put("TESTNET__SERVICE_KEY", e.testnet_service_key.clone());
    put("TESTNET__ENV_KEY", e.testnet_env_key.clone());
    Ok(env)
}

pub fn ready_url(m: &Manifest) -> Option<String> {
    m.embers.as_ref().map(|e| format!("http://127.0.0.1:{}/api/service/ready", e.port))
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_points_at_loopback_and_carries_every_key() {
        let mut m = crate::manifest::tests_support::sample();
        let e = super::EmbersSecrets::generate();
        let (svc, tsvc) = super::accounts(&e).unwrap();
        m.embers = Some(crate::manifest::Embers { port: 40600, service: svc, testnet_service: tsvc, funded: false, validator_slot: Some(2) });
        let env = super::env(&m, &e, 2).unwrap();
        let get = |k: &str| env.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(get("EMBERS__MAINNET__DEPLOY_SERVICE_URL"), "http://127.0.0.1:40421");
        assert_eq!(get("EMBERS__TESTNET__OBSERVER_URL"), "http://127.0.0.1:40453");
        assert_eq!(get("EMBERS__AES_ENCRYPTION_KEY").len(), 64);
        assert!(env.iter().all(|(_, v)| !v.contains("0.0.0.0")));
    }
}
