//! Integration with F1R3Gaze (spec §9 and S2, S8).
//!
//! Settings: F1R3Gaze reads `settings.conf` top to bottom and the last
//! assignment of a key wins, so a block that ign1t10n owns at the end of the
//! file governs the keys it sets. With work package G3 (`settings.d`), the
//! same lines go in `settings.d/50-ign1t10n.conf` instead and the block is
//! removed. Nothing outside the block is ever edited.
//!
//! Where deploys go (Decision 5, random selection): ign1t10n draws a random
//! active validator and writes it as `validator`. It redraws at every shard
//! start and after every resize, and never leaves a validator that is being
//! withdrawn in the setting. With work package G4 (`validator` taking a
//! list, one chosen at random per deploy), it writes every active validator.

use crate::manifest::Manifest;
use crate::paths::{Paths, write_atomic};
use crate::ports::Block;
use rand::seq::SliceRandom;
use std::path::Path;
use std::process::Command;

pub const BEGIN: &str = "# >>> managed by ign1t10n: do not edit inside this block >>>";
pub const END: &str = "# <<< managed by ign1t10n <<<";
pub const DROP_IN: &str = "settings.d/50-ign1t10n.conf";

/// F1R3Gaze capabilities that change what ign1t10n writes (versions.toml).
#[derive(Clone, Debug, Default)]
pub struct GazeFeatures {
    pub porcelain: bool,  // G1
    pub settings_d: bool, // G3
    pub validator_list: bool, // G4
}

impl GazeFeatures {
    pub fn from_env() -> GazeFeatures {
        let has = std::env::var("IGN1T10N_GAZE_HAS").unwrap_or_else(|_| option_env!("IGN1T10N_GAZE_HAS").unwrap_or("").into());
        let h = |k: &str| has.split(',').any(|x| x.trim() == k);
        GazeFeatures { porcelain: h("G1"), settings_d: h("G3"), validator_list: h("G4") }
    }
}

/// The URLs of validators that can take deploys: active, not retiring.
pub fn deploy_candidates(m: &Manifest) -> Vec<String> {
    m.active().iter().map(|v| Block(v.base_port).http_url()).collect()
}

/// Choose what to write as F1R3Gaze's `validator` setting.
pub fn choose_validators(m: &Manifest, f: &GazeFeatures, rng: &mut impl rand::Rng) -> Vec<String> {
    let mut c = deploy_candidates(m);
    if c.is_empty() {
        return vec![];
    }
    match m.options.deploy_target {
        crate::manifest::DeployTarget::First => c.truncate(1),
        crate::manifest::DeployTarget::Random => {
            c.shuffle(rng);
            if !f.validator_list {
                c.truncate(1);
            }
        }
    }
    c
}

pub fn block_lines(m: &Manifest, validators: &[String]) -> String {
    let mut s = String::new();
    s += &format!("validator = {}\n", validators.join(", "));
    s += &format!("observers = {}\n", Block(m.observer.base_port).http_url());
    s += &format!("shard_id = {}\n", m.shard.id);
    s += "quorum = 1\n";
    s += "phlo_price = 1\n";
    match &m.embers {
        Some(e) if m.options.embers => s += &format!("embers_api = http://127.0.0.1:{}\n", e.port),
        // An empty value clears any embers_api the user set, while the block governs.
        _ => {}
    }
    s
}

/// Remove the managed block from settings text.
pub fn strip_block(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == BEGIN {
            inside = true;
            continue;
        }
        if inside {
            if line.trim() == END {
                inside = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Replace (or append) the managed block.
pub fn with_block(text: &str, lines: &str) -> String {
    let mut s = strip_block(text);
    while s.ends_with("\n\n") {
        s.pop();
    }
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    format!("{s}{BEGIN}\n{lines}{END}\n")
}

/// F1R3Gaze's template, written if `settings.conf` is absent (as F1R3Gaze
/// itself would on first load).
const GAZE_TEMPLATE: &str = "# F1R3Gaze settings. Lists are comma-separated.\n# home = gaze://newtab\n# observers = https://observer-1.example, https://observer-2.example, https://observer-3.example\n# validator = https://validator.example\n# shard_id = root\n# quorum = 2\n# mirrors = https://cdn.example/blob/\n# https_only = false\n# Wallet balances, history and transfers (the Embers service F1R3Sky uses):\n# embers_api = https://embers.example\n# max_fee = 10000000\n";

/// Does the profile point F1R3Gaze somewhere other than loopback, outside our
/// block? (S8 asks before overriding that.)
pub fn points_elsewhere(profile: &Path) -> bool {
    let text = std::fs::read_to_string(profile.join("settings.conf")).unwrap_or_default();
    strip_block(&text).lines().map(str::trim).filter(|l| !l.starts_with('#')).any(|l| {
        let Some((k, v)) = l.split_once('=') else { return false };
        matches!(k.trim(), "validator" | "observers") && v.split(',').any(|u| {
            let u = u.trim();
            !u.is_empty() && !(u.contains("://127.0.0.1") || u.contains("://localhost") || u.contains("://[::1]"))
        })
    })
}

/// Write ign1t10n's settings into the profile.
pub fn apply(profile: &Path, m: &Manifest, validators: &[String], f: &GazeFeatures) -> Result<(), String> {
    std::fs::create_dir_all(profile).map_err(|e| e.to_string())?;
    let lines = block_lines(m, validators);
    let sc = profile.join("settings.conf");
    let text = std::fs::read_to_string(&sc).unwrap_or_else(|_| GAZE_TEMPLATE.to_string());
    if f.settings_d {
        let body = format!("# Written by ign1t10n; removed on uninstall.\n{lines}");
        write_atomic(&profile.join(DROP_IN), body.as_bytes(), 0o644).map_err(|e| e.to_string())?;
        if text.contains(BEGIN) {
            write_atomic(&sc, strip_block(&text).as_bytes(), 0o644).map_err(|e| e.to_string())?;
        }
        Ok(())
    } else {
        write_atomic(&sc, with_block(&text, &lines).as_bytes(), 0o644).map_err(|e| e.to_string())
    }
}

/// Remove ign1t10n's settings (toggle off, uninstall).
pub fn remove(profile: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(profile.join(DROP_IN));
    let sc = profile.join("settings.conf");
    if let Ok(text) = std::fs::read_to_string(&sc) {
        if text.contains(BEGIN) {
            write_atomic(&sc, strip_block(&text).as_bytes(), 0o644).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Choose and write, recording the choice in the manifest.
pub fn refresh(p: &Paths, m: &mut Manifest) -> Result<(), String> {
    if !m.options.gaze_integration {
        return remove(&p.profile);
    }
    let f = GazeFeatures::from_env();
    let v = choose_validators(m, &f, &mut rand::thread_rng());
    if v.is_empty() {
        return Ok(());
    }
    apply(&p.profile, m, &v, &f)?;
    m.wallet.gaze_validators = v;
    Ok(())
}

fn gaze(p: &Paths, args: &[&str]) -> Result<String, String> {
    let out = Command::new(p.gaze_bin())
        .arg("--profile")
        .arg(&p.profile)
        .args(args)
        .output()
        .map_err(|e| format!("{}: {e}", p.gaze_bin().display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("f1r3gaze {}: exit {:?}: {}", args.join(" "), out.status.code(), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Parse `f1r3gaze wallet list` (`* ADDRESS  LABEL` marks the active one).
pub fn active_from_list(list: &str) -> Option<String> {
    list.lines().find_map(|l| l.strip_prefix("* ")).and_then(|r| r.split_whitespace().next()).map(str::to_string)
}

/// S2: the address of the wallet F1R3Gaze will pay with, creating one if
/// there is none. ign1t10n never sees a wallet key.
pub fn paying_wallet(p: &Paths) -> Result<String, String> {
    let f = GazeFeatures::from_env();
    let addr = if f.porcelain {
        match gaze(p, &["wallet", "active", "--porcelain"]) {
            Ok(a) if !a.trim().is_empty() => Some(a.trim().to_string()),
            _ => None,
        }
    } else {
        active_from_list(&gaze(p, &["wallet", "list"])?)
    };
    let addr = match addr {
        Some(a) => a,
        None => {
            let args: &[&str] = if f.porcelain { &["wallet", "new", "Local shard", "--porcelain"] } else { &["wallet", "new", "Local shard"] };
            let out = gaze(p, args)?;
            let a = out.lines().map(str::trim).rfind(|l| l.starts_with("1111")).ok_or_else(|| format!("f1r3gaze wallet new printed no address: {out}"))?.to_string();
            // Before G1, `new` does not make the wallet active unless it is the first.
            if !f.porcelain && active_from_list(&gaze(p, &["wallet", "list"])?).as_deref() != Some(&a) {
                gaze(p, &["wallet", "use", &a])?;
            }
            a
        }
    };
    gaze_wallet::Address::parse(&addr).map(|a| a.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests_support::sample;
    use rand::SeedableRng;

    #[test]
    fn block_replaces_and_governs() {
        let m = sample();
        let user = "home = gaze://newtab\nvalidator = https://remote.example\n";
        let once = with_block(user, &block_lines(&m, &["http://127.0.0.1:40413".into()]));
        let twice = with_block(&once, &block_lines(&m, &["http://127.0.0.1:40423".into()]));
        assert_eq!(twice.matches(BEGIN).count(), 1);
        assert!(twice.starts_with(user), "the user's lines are untouched");
        let last_validator = twice.lines().filter(|l| l.starts_with("validator")).last().unwrap();
        assert_eq!(last_validator, "validator = http://127.0.0.1:40423");
        assert_eq!(strip_block(&twice), user);
    }

    #[test]
    fn random_selection_uses_every_active_validator() {
        let mut m = sample();
        m.validators[1].state = crate::manifest::SlotState::Retiring;
        let f = GazeFeatures::default();
        let mut r = rand::rngs::StdRng::seed_from_u64(1);
        for _ in 0..20 {
            assert_eq!(choose_validators(&m, &f, &mut r), vec!["http://127.0.0.1:40413".to_string()], "never a retiring validator");
        }
        m.validators[1].state = crate::manifest::SlotState::Active;
        let seen: std::collections::BTreeSet<_> = (0..50).map(|_| choose_validators(&m, &f, &mut r)[0].clone()).collect();
        assert_eq!(seen.len(), 2);
        let list = choose_validators(&m, &GazeFeatures { validator_list: true, ..Default::default() }, &mut r);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn wallet_list_parsing_and_elsewhere() {
        assert_eq!(active_from_list("  1111a  x\n* 1111b  Local shard\n").as_deref(), Some("1111b"));
        assert_eq!(active_from_list("  1111a  x\n"), None);
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("settings.conf"), "observers = https://obs.example\n").unwrap();
        assert!(points_elsewhere(d.path()));
        std::fs::write(d.path().join("settings.conf"), "validator = http://localhost:40403\n").unwrap();
        assert!(!points_elsewhere(d.path()));
    }
}
