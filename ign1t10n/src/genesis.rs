//! Genesis files (spec §6.5, Decision 4): `bonds.txt` holds one line per
//! genesis validator (never the bootstrap); `wallets.txt` funds the browser's
//! wallet with 10,000,000 F1R3, each validator's vault with its fees, the
//! Embers service wallets when Embers is bundled, and the faucet with the rest.

use crate::amounts::*;
use crate::manifest::Manifest;
use crate::paths::{Paths, write_atomic};

pub fn bonds(m: &Manifest) -> String {
    let mut s = String::new();
    for v in m.validators.iter().filter(|v| v.genesis) {
        s += &format!("{} {}\n", v.public_key, STAKE);
    }
    s
}

pub fn wallets(m: &Manifest) -> Result<String, String> {
    let wallet = m.wallet.funded.as_ref().ok_or("no F1R3Gaze wallet to fund")?;
    let mut lines = vec![(wallet.clone(), dust(BROWSER_WALLET_F1R3))];
    for v in m.validators.iter().filter(|v| v.genesis) {
        lines.push((v.address.clone(), dust(VALIDATOR_FEES_F1R3)));
    }
    if let Some(e) = m.embers.as_ref().filter(|_| m.options.embers) {
        lines.push((e.service.address.clone(), dust(EMBERS_SERVICE_F1R3)));
        lines.push((e.testnet_service.address.clone(), dust(EMBERS_SERVICE_F1R3)));
    }
    let used: i64 = lines.iter().map(|(_, b)| b).sum();
    let rest = dust(SUPPLY_F1R3) - used;
    if rest <= 0 {
        return Err("the genesis supply does not cover the allocations".into());
    }
    lines.push((m.faucet.address.clone(), rest));
    // The same address twice would merge silently at genesis; refuse instead.
    let mut seen = std::collections::BTreeSet::new();
    for (a, _) in &lines {
        if !seen.insert(a) {
            return Err(format!("address {a} appears twice in wallets.txt"));
        }
    }
    Ok(lines.iter().map(|(a, b)| format!("{a},{b}\n")).collect())
}

pub fn write(p: &Paths, m: &Manifest) -> Result<(), String> {
    let w = wallets(m)?;
    write_atomic(&p.genesis().join("bonds.txt"), bonds(m).as_bytes(), 0o600).map_err(|e| e.to_string())?;
    write_atomic(&p.genesis().join("wallets.txt"), w.as_bytes(), 0o600).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn supply_is_conserved() {
        let mut m = crate::manifest::tests_support::sample();
        let w = super::wallets(&m).unwrap();
        let total: i64 = w.lines().map(|l| l.split(',').nth(1).unwrap().parse::<i64>().unwrap()).sum();
        assert_eq!(total, super::dust(super::SUPPLY_F1R3));
        assert!(w.starts_with(&format!("1111w,{}\n", 1_000_000_000_000_000i64)), "10M F1R3 to the browser wallet");
        assert_eq!(super::bonds(&m).lines().count(), 2);
        m.faucet.address = "1111w".into();
        assert!(super::wallets(&m).is_err());
    }
}
