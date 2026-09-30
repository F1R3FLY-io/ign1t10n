//! `shard.toml`, the manifest (spec Appendix B, schema 3). It records the
//! shard's identity, every node's key and ports, each validator slot's
//! lifecycle state, the options chosen at installation, and the progress of
//! provisioning and of any resize, so that every operation resumes after an
//! interruption.

use crate::paths::{Paths, write_atomic};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    pub shard: Shard,
    pub options: Options,
    pub bootstrap: Keyed,
    pub observer: Observer,
    pub faucet: Account,
    pub wallet: Wallet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embers: Option<Embers>,
    #[serde(default)]
    pub validators: Vec<Validator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize: Option<Resize>,
    #[serde(default)]
    pub stages: Stages,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Shard {
    pub id: String,
    pub network_id: String,
    /// N0, the number of genesis validators.
    pub genesis_validators: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_node_version: Option<String>,
    /// Work package N4; absent until the node reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_format: Option<u32>,
    pub created: String,
}

/// What the person chose on the installation configuration panel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Options {
    /// Bundle and run Embers (Decision 8; default on).
    pub embers: bool,
    /// Point F1R3Gaze at the local shard.
    pub gaze_integration: bool,
    /// Where F1R3Gaze's deploys go (Decision 5).
    pub deploy_target: DeployTarget,
    /// The timing profile rendered into `common.conf` (Decision 6).
    pub timing: String,
}

impl Default for Options {
    fn default() -> Self {
        Options { embers: true, gaze_integration: true, deploy_target: DeployTarget::Random, timing: crate::nodeconf::TIMING_PROFILE.into() }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeployTarget {
    /// A randomly selected active validator (Decision 5).
    Random,
    /// Always validator 1.
    First,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub public_key: String,
    pub address: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Keyed {
    pub public_key: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub base_port: u16,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Observer {
    pub base_port: u16,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Wallet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funded: Option<String>,
    /// The validator URL(s) currently written into F1R3Gaze's settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaze_validators: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Embers {
    pub port: u16,
    pub service: Account,
    pub testnet_service: Account,
    /// Funded at genesis, or later from the faucet.
    pub funded: bool,
    /// The validator slot Embers deploys to, drawn at each start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator_slot: Option<u8>,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum SlotState {
    Absent,
    Keyed,
    Funded,
    Joining,
    Bonding,
    Active,
    Retiring,
    Left,
    Paid,
}

impl SlotState {
    /// States in which the node process runs.
    pub fn runs(self) -> bool {
        matches!(self, SlotState::Joining | SlotState::Bonding | SlotState::Active | SlotState::Retiring)
    }
    pub fn name(self) -> &'static str {
        match self {
            SlotState::Absent => "absent",
            SlotState::Keyed => "keyed",
            SlotState::Funded => "funded",
            SlotState::Joining => "joining",
            SlotState::Bonding => "bonding",
            SlotState::Active => "active",
            SlotState::Retiring => "retiring",
            SlotState::Left => "left",
            SlotState::Paid => "paid",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Validator {
    pub slot: u8,
    pub public_key: String,
    pub address: String,
    pub base_port: u16,
    pub state: SlotState,
    pub genesis: bool,
    /// The deploy id of the step in flight (bond, withdraw, transfer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<String>,
    /// Vault balance before withdrawal, to recognise the payout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_before_payout: Option<i64>,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResizeMode {
    InPlace,
    NewShard,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Resize {
    pub target: u8,
    pub mode: ResizeMode,
    pub started: String,
    /// Slots to grow (ascending) or shrink (descending), in order.
    pub slots: Vec<u8>,
    pub grow: bool,
    /// Set when a step failed; the resize waits for Retry or Undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// True while undoing.
    #[serde(default)]
    pub undo: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Stages {
    #[serde(default)] pub preflight: bool,
    #[serde(default)] pub ports: bool,
    #[serde(default)] pub wallet: bool,
    #[serde(default)] pub keys: bool,
    #[serde(default)] pub genesis_files: bool,
    #[serde(default)] pub configs: bool,
    #[serde(default)] pub agent: bool,
    #[serde(default)] pub genesis: bool,
    #[serde(default)] pub gaze: bool,
    #[serde(default)] pub opened: bool,
}

impl Stages {
    pub fn complete(&self) -> bool {
        self.preflight && self.ports && self.wallet && self.keys && self.genesis_files && self.configs && self.agent && self.genesis && self.gaze
    }
    /// Ready for the supervisor: files exist, genesis may still be pending.
    pub fn prepared(&self) -> bool {
        self.ports && self.wallet && self.keys && self.genesis_files && self.configs
    }
}

impl Manifest {
    pub fn load(p: &Paths) -> Result<Option<Manifest>, String> {
        match std::fs::read_to_string(p.manifest()) {
            Ok(t) => {
                let m: Manifest = toml::from_str(&t).map_err(|e| format!("shard.toml: {e}"))?;
                if m.schema > SCHEMA {
                    return Err(format!("shard.toml has schema {}, newer than this ign1t10n ({SCHEMA})", m.schema));
                }
                Ok(Some(m))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("shard.toml: {e}")),
        }
    }

    pub fn save(&self, p: &Paths) -> Result<(), String> {
        let t = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        let body = format!("# ~/Library/Application Support/F1R3FLY/ign1t10n/shard.toml\n# Written by ign1t10n. Do not edit while the shard runs.\n{t}");
        write_atomic(&p.manifest(), body.as_bytes(), 0o600).map_err(|e| format!("shard.toml: {e}"))
    }

    pub fn validator(&self, slot: u8) -> Option<&Validator> {
        self.validators.iter().find(|v| v.slot == slot)
    }
    pub fn validator_mut(&mut self, slot: u8) -> Option<&mut Validator> {
        self.validators.iter_mut().find(|v| v.slot == slot)
    }
    /// Validators in the active set.
    pub fn active(&self) -> Vec<&Validator> {
        self.validators.iter().filter(|v| v.state == SlotState::Active).collect()
    }
    /// N: the number of validators counted by the panel (active or on their
    /// way in or out).
    pub fn n(&self) -> u8 {
        self.validators.iter().filter(|v| v.state.runs()).count() as u8
    }
    /// Every port this shard has allocated.
    pub fn taken_ports(&self) -> Vec<u16> {
        let mut t = Vec::new();
        let mut add = |b: u16| t.extend(crate::ports::Block(b).all());
        add(self.bootstrap.base_port);
        add(self.observer.base_port);
        for v in &self.validators {
            if v.state != SlotState::Absent {
                add(v.base_port);
            }
        }
        if let Some(e) = &self.embers {
            t.push(e.port);
        }
        t
    }
    pub fn free_slot(&self) -> Option<u8> {
        (1..=crate::MAX_VALIDATORS).find(|s| self.validator(*s).map(|v| v.state == SlotState::Absent).unwrap_or(true))
    }
}

#[cfg(test)]
pub mod tests_support {
    use super::*;

    pub fn sample() -> Manifest {
        Manifest {
            schema: SCHEMA,
            shard: Shard { id: "root".into(), network_id: "ign1t10n-3f9c1a07".into(), genesis_validators: 2, created: "2026-10-14T09:12:44Z".into(), ..Default::default() },
            options: Options::default(),
            bootstrap: Keyed { public_key: "04e2".into(), address: "1111b".into(), node_id: None, base_port: 40400 },
            observer: Observer { base_port: 40450 },
            faucet: Account { public_key: "04f".into(), address: "1111f".into() },
            wallet: Wallet { funded: Some("1111w".into()), gaze_validators: vec![] },
            embers: None,
            validators: (1..=2)
                .map(|s| Validator { slot: s, public_key: format!("04{s}"), address: format!("1111{s}"), base_port: crate::ports::validator_base(s), state: SlotState::Active, genesis: true, deploy: None, balance_before_payout: None })
                .collect(),
            resize: None,
            stages: Stages::default(),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use super::tests_support::sample;

    #[test]
    fn round_trips_through_toml() {
        let m = sample();
        let t = toml::to_string_pretty(&m).unwrap();
        assert!(t.contains("deploy_target = \"random\""));
        let back: Manifest = toml::from_str(&t).unwrap();
        assert_eq!(back, m);
        assert_eq!(m.n(), 2);
        assert_eq!(m.free_slot(), Some(3));
    }
}
