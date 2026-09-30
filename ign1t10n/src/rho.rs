//! The rholang ign1t10n deploys. Transfers use the node's system vault
//! (`rholang/examples/vault_demo/3.transfer_funds.rho`), bond and withdraw
//! the PoS contract (`rholang/examples/bond/bond.rho`,
//! `casper/src/main/resources/PoS.rhox`). Every term is fixed text with
//! checked arguments: addresses are validated F1R3Cap addresses and amounts
//! are integers, so nothing can be injected.

use gaze_wallet::Address;

pub fn transfer(from: &Address, to: &Address, amount: i64) -> String {
    assert!(amount > 0);
    format!(
        r#"new rl(`rho:registry:lookup`), vaultSysCh, deployerId(`rho:system:deployerId`) in {{
  rl!(`rho:vault:system`, *vaultSysCh) |
  for (@(_, SystemVault) <- vaultSysCh) {{
    new vaultCh, targetCh, keyCh, resultCh in {{
      @SystemVault!("findOrCreate", "{from}", *vaultCh) |
      @SystemVault!("findOrCreate", "{to}", *targetCh) |
      @SystemVault!("deployerAuthKey", *deployerId, *keyCh) |
      for (@(true, vault) <- vaultCh & key <- keyCh & @(true, _) <- targetCh) {{
        @vault!("transfer", "{to}", {amount}, *key, *resultCh) |
        for (@_ <- resultCh) {{ Nil }}
      }}
    }}
  }}
}}"#
    )
}

pub fn bond(stake: i64) -> String {
    assert!(stake > 0);
    format!(
        r#"new retCh, PoSCh, rl(`rho:registry:lookup`), deployerId(`rho:system:deployerId`) in {{
  rl!(`rho:system:pos`, *PoSCh) |
  for (@(_, PoS) <- PoSCh) {{
    @PoS!("bond", *deployerId, {stake}, *retCh) |
    for (@_ <- retCh) {{ Nil }}
  }}
}}"#
    )
}

pub fn withdraw() -> String {
    r#"new retCh, PoSCh, rl(`rho:registry:lookup`), deployerId(`rho:system:deployerId`) in {
  rl!(`rho:system:pos`, *PoSCh) |
  for (@(_, PoS) <- PoSCh) {
    @PoS!("withdraw", *deployerId, *retCh) |
    for (@_ <- retCh) { Nil }
  }
}"#
    .to_string()
}

/// Exploratory: the active validator set, returned on the first `new` name.
pub fn active_validators() -> String {
    r#"new return, rl(`rho:registry:lookup`), PoSCh in {
  rl!(`rho:system:pos`, *PoSCh) |
  for (@(_, PoS) <- PoSCh) { @PoS!("getActiveValidators", *return) }
}"#
    .to_string()
}

/// Does an exploratory answer mention this public key? The node renders byte
/// arrays as hex; searching the JSON text is independent of the exact shape.
pub fn mentions_key(answer: &serde_json::Value, public_key: &str) -> bool {
    answer.to_string().to_ascii_lowercase().contains(&public_key.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    #[test]
    fn terms_are_closed_and_parse_shaped() {
        let k = crate::keys::Key::generate();
        let a = gaze_wallet::Address::parse(&k.address()).unwrap();
        let t = super::transfer(&a, &a, 5);
        assert!(t.contains(&format!("\"{a}\"")) && t.contains(", 5, *key"));
        for term in [t, super::bond(1000), super::withdraw(), super::active_validators()] {
            assert_eq!(term.matches('{').count(), term.matches('}').count());
            assert!(!term.contains("rho:rchain:deployerId"));
        }
    }
}
