//! Node configuration (spec Appendix A): `common.conf` carries what every
//! node shares; one small file per node includes it and sets its ports, data
//! directory and role.
//!
//! Decision 6 (timings tuned for a laptop): the shard's upstream values
//! (`docker/conf/default.conf`: casper loop 750 ms, heartbeat on, 5 s maximum
//! LFB age) are relaxed in proportion to the number of validators, so that
//! ten validators, an observer, a bootstrap and Embers idle quietly on one
//! machine. The profile has a name, recorded in `shard.toml`; T1 recalibrates
//! it from measured finalisation latency and idle CPU and bumps the name.
//!
//! Decision 7 (refuse foreign Origin): `api-server.reject-foreign-origin` asks
//! the node (work package N5) to answer 403 to any HTTP request carrying an
//! `Origin` header, which every browser page sends and F1R3Gaze and Embers
//! never do. Work package N1's bind addresses are written likewise. The node
//! ignores keys it does not know, so both are written unconditionally and the
//! supervisor audits the listening sockets.

use crate::manifest::Manifest;
use crate::paths::{Paths, write_atomic};
use crate::ports::Block;

pub const TIMING_PROFILE: &str = "laptop-v1";

/// Approvals the genesis ceremony waits for. The node requires strictly fewer
/// than the number of genesis validators (`approve_block_protocol.rs`: "Required
/// sigs must be smaller than the number of bonded validators"); its own Docker
/// shard uses 2 of 3. So N0 − 1, at least 1.
pub fn required_signatures(n0: u8) -> u8 {
    n0.saturating_sub(1).max(1)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    pub casper_loop_ms: u64,
    pub requested_blocks_timeout_s: u64,
    pub fork_choice_stale_threshold_s: u64,
    pub fork_choice_check_interval_s: u64,
    pub heartbeat_check_interval_s: u64,
    pub heartbeat_max_lfb_age_s: u64,
    pub heartbeat_self_propose_cooldown_s: u64,
    pub discovery_lookup_interval_s: u64,
}

/// The `laptop-v1` profile for `n` validators.
pub fn timing(n: u8) -> Timing {
    let n = n.clamp(crate::MIN_VALIDATORS, crate::MAX_VALIDATORS) as u64;
    Timing {
        // 1.0 s at N = 2, rising 250 ms per further validator to 3.0 s at N = 10.
        casper_loop_ms: 1000 + 250 * (n - 2),
        requested_blocks_timeout_s: 10,
        fork_choice_stale_threshold_s: 60,
        fork_choice_check_interval_s: 30,
        // An idle shard proposes an empty block only when the LFB is older
        // than this, so it bounds idle work; deploys still propose at once.
        heartbeat_check_interval_s: if n <= 4 { 5 } else { 8 },
        heartbeat_max_lfb_age_s: if n <= 4 { 15 } else { 30 },
        heartbeat_self_propose_cooldown_s: 3,
        discovery_lookup_interval_s: 60,
    }
}

pub fn common(p: &Paths, m: &Manifest) -> String {
    let t = timing(m.n().max(m.shard.genesis_validators));
    let state = p.state.display();
    format!(
        r#"# Rendered by ign1t10n ({profile}). The node loads its built-in defaults.conf first;
# these are docker/conf/default.conf's values, generalised to loopback and tuned for a laptop.
protocol-server {{
  network-id = "{net}"
  bind-address = "127.0.0.1"
  allow-private-addresses = true
  no-upnp = true
}}
peers-discovery {{
  bind-address = "127.0.0.1"
  lookup-interval = {lookup} seconds
}}
api-server {{
  host = "127.0.0.1"
  reject-foreign-origin = true
}}
casper {{
  shard-name = "{shard}"
  fault-tolerance-threshold = 0.1
  casper-loop-interval = {loop_ms} ms
  requested-blocks-timeout = {rbt} seconds
  fork-choice-stale-threshold = {fcst} seconds
  fork-choice-check-if-stale-interval = {fcci} seconds
  max-number-of-parents = 100
  enable-mergeable-channel-gc = true
  heartbeat {{
    enabled = true
    check-interval = {hci} seconds
    max-lfb-age = {hla} seconds
    self-propose-cooldown = {hsc} seconds
  }}
  genesis-ceremony {{
    required-signatures = {sigs}
    approve-interval = 10 seconds
    approve-duration = 1 minutes
  }}
  genesis-block-data {{
    genesis-data-dir = "{state}/genesis"
    bonds-file = "{state}/genesis/bonds.txt"
    wallets-file = "{state}/genesis/wallets.txt"
    number-of-active-validators = {max}
    epoch-length = 10
    quarantine-length = 10
  }}
}}
logging {{
  format = "json"
  # To standard output, which ign1t10n writes to <node>.stdout.log (rotated),
  # so a node's own errors land where ign1t10n's messages point.
  sink = "stdout"
}}
metrics {{ prometheus = true }}
"#,
        profile = m.options.timing,
        net = m.shard.network_id,
        lookup = t.discovery_lookup_interval_s,
        shard = m.shard.id,
        loop_ms = t.casper_loop_ms,
        rbt = t.requested_blocks_timeout_s,
        fcst = t.fork_choice_stale_threshold_s,
        fcci = t.fork_choice_check_interval_s,
        hci = t.heartbeat_check_interval_s,
        hla = t.heartbeat_max_lfb_age_s,
        hsc = t.heartbeat_self_propose_cooldown_s,
        sigs = required_signatures(m.shard.genesis_validators),
        max = crate::MAX_VALIDATORS,
    )
}

pub fn node(p: &Paths, name: &str, base: u16, extra: &str) -> String {
    let b = Block(base);
    let dir = p.node_dir(name);
    let d = dir.display();
    format!(
        r#"# Rendered by ign1t10n: {name}.
include "common.conf"
{extra}protocol-server.port = {proto}
peers-discovery.port = {disc}
api-server {{
  port-grpc-external = {ge}
  port-grpc-internal = {gi}
  port-http = {http}
  port-admin-http = {admin}
}}
storage.data-dir = "{d}"
tls {{
  certificate-path = "{d}/node.certificate.pem"
  key-path = "{d}/node.key.pem"
}}
"#,
        proto = b.protocol(),
        disc = b.discovery(),
        ge = b.grpc_external(),
        gi = b.grpc_internal(),
        http = b.http(),
        admin = b.admin(),
    )
}

pub fn validator_name(slot: u8) -> String {
    format!("validator-{slot}")
}

/// Render every configuration file for the manifest's current nodes.
pub fn write_all(p: &Paths, m: &Manifest) -> Result<(), String> {
    let w = |name: &str, body: String| write_atomic(&p.conf_file(name), body.as_bytes(), 0o600).map_err(|e| format!("{name}.conf: {e}"));
    w("common", common(p, m))?;
    w("bootstrap", node(p, "bootstrap", m.bootstrap.base_port, "casper.genesis-ceremony.ceremony-master-mode = true\n"))?;
    w("observer", node(p, "observer", m.observer.base_port, "casper.heartbeat.enabled = false\n"))?;
    for v in m.validators.iter().filter(|v| v.state != crate::manifest::SlotState::Absent) {
        let n = validator_name(v.slot);
        w(&n, node(p, &n, v.base_port, ""))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_signatures_are_fewer_than_genesis_validators() {
        for n0 in crate::MIN_VALIDATORS..=crate::MAX_VALIDATORS {
            let r = required_signatures(n0);
            assert!(r >= 1 && r < n0);
        }
    }

    #[test]
    fn timing_is_monotone_and_bounded() {
        assert_eq!(timing(2).casper_loop_ms, 1000);
        assert_eq!(timing(10).casper_loop_ms, 3000);
        assert!(timing(10).heartbeat_max_lfb_age_s > timing(2).heartbeat_max_lfb_age_s);
    }

    #[test]
    fn rendering_quotes_paths_and_sets_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Paths::from_env();
        p.state = dir.path().join("Application Support/ign1t10n");
        let m = crate::manifest::tests_support::sample();
        let c = common(&p, &m);
        assert!(c.contains("host = \"127.0.0.1\""));
        assert!(c.contains("reject-foreign-origin = true"));
        assert!(c.contains("required-signatures = 1"), "fewer approvals than genesis validators");
        assert!(c.contains("sink = \"stdout\""));
        assert!(c.contains(&format!("\"{}/genesis/bonds.txt\"", p.state.display())));
        let v = node(&p, "validator-1", 40410, "");
        assert!(v.contains("port-http = 40413"));
        assert!(v.contains("include \"common.conf\""));
    }
}
