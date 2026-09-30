//! Genesis amounts and stakes (Decision 4: the browser's wallet receives
//! 10,000,000 F1R3). Balances are in dust; F1R3 has 8 decimals.

pub const DECIMALS: u32 = 8;
pub const DUST_PER_F1R3: i64 = 100_000_000;

/// Total supply the local genesis distributes.
pub const SUPPLY_F1R3: i64 = 500_000_000;
/// The browser's active wallet: 10M F1R3 (Decision 4).
pub const BROWSER_WALLET_F1R3: i64 = 10_000_000;
/// Each validator's vault: fees for its own bond, withdraw and sweep.
pub const VALIDATOR_FEES_F1R3: i64 = 1_000;
/// Each validator's bond, in the unit `bonds.txt` and `PoS!("bond")` use.
pub const STAKE: i64 = 1_000;
/// The Embers service wallets (mainnet-side and testnet-side services).
pub const EMBERS_SERVICE_F1R3: i64 = 1_000_000;
/// Phlo for administrative deploys.
pub const ADMIN_PHLO_LIMIT: i64 = 5_000_000;
pub const PHLO_PRICE: i64 = 1;

pub fn dust(f1r3: i64) -> i64 {
    f1r3 * DUST_PER_F1R3
}

/// Format dust as F1R3 with trailing zeros trimmed.
pub fn show(dust: i64) -> String {
    let whole = dust / DUST_PER_F1R3;
    let frac = (dust % DUST_PER_F1R3).abs();
    if frac == 0 {
        format!("{whole} F1R3")
    } else {
        let f = format!("{frac:08}");
        format!("{whole}.{} F1R3", f.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formatting() {
        assert_eq!(super::show(super::dust(10_000_000)), "10000000 F1R3");
        assert_eq!(super::show(150_000_000), "1.5 F1R3");
    }
}
