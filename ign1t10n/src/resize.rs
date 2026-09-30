//! Resizing the running shard (spec §8). Each validator slot moves through
//! `absent → keyed → funded → joining → bonding → active` when it grows and
//! `active → retiring → left → paid → absent` when it shrinks. The engine is
//! pure: every effect (keys, deploys, processes, waiting) goes through
//! [`Ops`], and every state change is persisted by `Ops::set_state` before
//! the next step, so a resize interrupted anywhere resumes where it stopped.
//!
//! Invariants: 2 ≤ N ≤ 10 at every step; validator 1 is never removed; a
//! bonded validator is stopped only after it has left the active set; at
//! most one resize runs at a time.

use crate::manifest::{Manifest, Resize, ResizeMode, SlotState};

pub trait Ops {
    fn state(&self, slot: u8) -> SlotState;
    /// Persist a slot's new state (and the manifest with it).
    fn set_state(&mut self, slot: u8, s: SlotState) -> Result<(), String>;
    fn progress(&mut self, slot: u8, msg: &str);

    /// Ports, key, PEM, Keychain item, configuration.
    fn prepare(&mut self, slot: u8) -> Result<(), String>;
    /// Faucet transfer of stake plus fees; wait until finalised and visible.
    fn fund(&mut self, slot: u8) -> Result<(), String>;
    /// Start the node (no --genesis-validator) and wait for /api/ready.
    fn start_and_wait(&mut self, slot: u8) -> Result<(), String>;
    /// Sign the bond with the joiner's key, send it to another active
    /// validator, wait until finalised.
    fn bond(&mut self, slot: u8) -> Result<(), String>;
    /// Wait for /api/bond-status to report the key bonded (next epoch).
    fn await_bonded(&mut self, slot: u8) -> Result<(), String>;
    /// Move F1R3Gaze and Embers off this validator if they use it, then sign
    /// and send the withdraw, wait until finalised.
    fn withdraw(&mut self, slot: u8) -> Result<(), String>;
    /// Wait until the key is no longer in the active set.
    fn await_left(&mut self, slot: u8) -> Result<(), String>;
    fn stop(&mut self, slot: u8) -> Result<(), String>;
    /// Wait for the stake to be paid out after the quarantine.
    fn await_payout(&mut self, slot: u8) -> Result<(), String>;
    /// Transfer the vault, less a fee, back to the faucet (best effort).
    fn sweep(&mut self, slot: u8) -> Result<(), String>;
    /// Archive data directory, configuration and PEM; delete the Keychain item.
    fn archive(&mut self, slot: u8) -> Result<(), String>;
}

/// Validate a request and produce the plan.
pub fn plan(m: &Manifest, target: u8, new_shard: bool) -> Result<Resize, String> {
    if !(crate::MIN_VALIDATORS..=crate::MAX_VALIDATORS).contains(&target) {
        return Err(format!("the shard has between {} and {} validators", crate::MIN_VALIDATORS, crate::MAX_VALIDATORS));
    }
    if m.resize.is_some() {
        return Err("a resize is already in progress".into());
    }
    let n = m.n();
    let started = crate::now_rfc3339();
    if new_shard {
        return Ok(Resize { target, mode: ResizeMode::NewShard, started, slots: vec![], grow: target > n, failed: None, undo: false });
    }
    if target == n {
        return Err(format!("the shard already has {n} validators"));
    }
    let slots = if target > n {
        let mut free: Vec<u8> = (1..=crate::MAX_VALIDATORS)
            .filter(|s| m.validator(*s).map(|v| v.state == SlotState::Absent).unwrap_or(true))
            .collect();
        free.truncate((target - n) as usize);
        free
    } else {
        let mut occ: Vec<u8> = m.validators.iter().filter(|v| v.state == SlotState::Active && v.slot != 1).map(|v| v.slot).collect();
        occ.sort_unstable_by(|a, b| b.cmp(a));
        occ.truncate((n - target) as usize);
        occ
    };
    if slots.len() != (target as i16 - n as i16).unsigned_abs() as usize {
        return Err("not enough slots in a state that can change; finish or undo the current changes first".into());
    }
    Ok(Resize { target, mode: ResizeMode::InPlace, started, slots, grow: target > n, failed: None, undo: false })
}

/// One step of growing a slot. Returns false when the slot is done.
fn grow_step(ops: &mut dyn Ops, slot: u8) -> Result<bool, String> {
    use SlotState::*;
    let next = match ops.state(slot) {
        Absent => {
            ops.progress(slot, "generating key");
            ops.prepare(slot)?;
            Keyed
        }
        Keyed => {
            ops.progress(slot, "funding from the faucet");
            ops.fund(slot)?;
            Funded
        }
        Funded => {
            ops.progress(slot, "joining the shard");
            ops.start_and_wait(slot)?;
            Joining
        }
        Joining => {
            ops.progress(slot, "sending bond");
            ops.bond(slot)?;
            Bonding
        }
        Bonding => {
            ops.progress(slot, "waiting for the next epoch");
            ops.await_bonded(slot)?;
            Active
        }
        Active => return Ok(false),
        // Undoing a shrink: finish leaving, then come back as a new validator.
        Retiring | Left | Paid => return shrink_step(ops, slot).map(|_| true),
    };
    ops.set_state(slot, next)?;
    Ok(true)
}

/// One step of shrinking a slot. Returns false when the slot is absent.
fn shrink_step(ops: &mut dyn Ops, slot: u8) -> Result<bool, String> {
    use SlotState::*;
    if slot == 1 {
        return Err("validator 1 is never removed".into());
    }
    let next = match ops.state(slot) {
        Active => {
            ops.progress(slot, "sending withdraw");
            ops.withdraw(slot)?;
            Retiring
        }
        Retiring => {
            ops.progress(slot, "waiting to leave the active set");
            ops.await_left(slot)?;
            ops.stop(slot)?;
            Left
        }
        Left => {
            ops.progress(slot, "waiting for the payout");
            ops.await_payout(slot)?;
            if let Err(e) = ops.sweep(slot) {
                ops.progress(slot, &format!("sweep to the faucet failed ({e}); continuing"));
            }
            Paid
        }
        Paid | Keyed => {
            ops.archive(slot)?;
            Absent
        }
        Funded | Joining => {
            // Undoing a grow before the bond: not bonded, so it may stop now.
            ops.stop(slot)?;
            let _ = ops.sweep(slot);
            ops.archive(slot)?;
            Absent
        }
        // A bond in flight may still land: let it, then withdraw.
        Bonding => {
            ops.progress(slot, "bond in flight; waiting before withdrawing");
            ops.await_bonded(slot)?;
            Active
        }
        Absent => return Ok(false),
    };
    ops.set_state(slot, next)?;
    Ok(true)
}

/// Run (or resume) a plan. Slots are processed one after another.
pub fn run(ops: &mut dyn Ops, r: &Resize) -> Result<(), String> {
    for &slot in &r.slots {
        loop {
            let more = if r.grow { grow_step(ops, slot)? } else { shrink_step(ops, slot)? };
            if !more {
                break;
            }
        }
    }
    Ok(())
}

/// The plan that undoes `r`: the slots it already changed, in reverse.
pub fn undo_plan(ops: &dyn Ops, r: &Resize) -> Resize {
    let start = if r.grow { SlotState::Absent } else { SlotState::Active };
    let slots: Vec<u8> = r.slots.iter().rev().copied().filter(|s| ops.state(*s) != start).collect();
    Resize { target: 0, mode: r.mode, started: crate::now_rfc3339(), slots, grow: !r.grow, failed: None, undo: true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests_support::sample;
    use std::collections::BTreeMap;

    struct Mock {
        states: BTreeMap<u8, SlotState>,
        log: Vec<String>,
        fail_once: Option<(&'static str, u8)>,
        running: BTreeMap<u8, bool>,
        bonded: BTreeMap<u8, bool>,
    }

    impl Mock {
        fn new(n: u8) -> Mock {
            Mock {
                states: (1..=n).map(|s| (s, SlotState::Active)).collect(),
                log: vec![],
                fail_once: None,
                running: (1..=n).map(|s| (s, true)).collect(),
                bonded: (1..=n).map(|s| (s, true)).collect(),
            }
        }
        fn act(&mut self, what: &'static str, slot: u8) -> Result<(), String> {
            if self.fail_once == Some((what, slot)) {
                self.fail_once = None;
                return Err(format!("{what} failed"));
            }
            self.log.push(format!("{what}:{slot}"));
            Ok(())
        }
    }

    impl Ops for Mock {
        fn state(&self, s: u8) -> SlotState { *self.states.get(&s).unwrap_or(&SlotState::Absent) }
        fn set_state(&mut self, s: u8, st: SlotState) -> Result<(), String> { self.states.insert(s, st); Ok(()) }
        fn progress(&mut self, _: u8, _: &str) {}
        fn prepare(&mut self, s: u8) -> Result<(), String> { self.act("prepare", s) }
        fn fund(&mut self, s: u8) -> Result<(), String> { self.act("fund", s) }
        fn start_and_wait(&mut self, s: u8) -> Result<(), String> { self.act("start", s)?; self.running.insert(s, true); Ok(()) }
        fn bond(&mut self, s: u8) -> Result<(), String> { self.act("bond", s)?; self.bonded.insert(s, true); Ok(()) }
        fn await_bonded(&mut self, s: u8) -> Result<(), String> { self.act("bonded", s) }
        fn withdraw(&mut self, s: u8) -> Result<(), String> { self.act("withdraw", s) }
        fn await_left(&mut self, s: u8) -> Result<(), String> { self.act("left", s)?; self.bonded.insert(s, false); Ok(()) }
        fn stop(&mut self, s: u8) -> Result<(), String> {
            assert!(!self.bonded.get(&s).copied().unwrap_or(false), "a bonded validator is never stopped on its own");
            self.running.insert(s, false);
            self.act("stop", s)
        }
        fn await_payout(&mut self, s: u8) -> Result<(), String> { self.act("payout", s) }
        fn sweep(&mut self, s: u8) -> Result<(), String> { self.act("sweep", s) }
        fn archive(&mut self, s: u8) -> Result<(), String> { self.act("archive", s) }
    }

    fn manifest_from(mock: &Mock) -> Manifest {
        let mut m = sample();
        m.validators = mock
            .states
            .iter()
            .map(|(s, st)| crate::manifest::Validator { slot: *s, public_key: format!("04{s}"), address: format!("1111{s}"), base_port: crate::ports::validator_base(*s), state: *st, genesis: *s <= 2, deploy: None, balance_before_payout: None })
            .collect();
        m
    }

    #[test]
    fn grow_2_to_5_then_shrink_to_3() {
        let mut ops = Mock::new(2);
        let p = plan(&manifest_from(&ops), 5, false).unwrap();
        assert_eq!(p.slots, vec![3, 4, 5]);
        run(&mut ops, &p).unwrap();
        assert!((1..=5).all(|s| ops.state(s) == SlotState::Active));
        assert_eq!(&ops.log[..5], &["prepare:3", "fund:3", "start:3", "bond:3", "bonded:3"]);

        let p = plan(&manifest_from(&ops), 3, false).unwrap();
        assert_eq!(p.slots, vec![5, 4], "highest slots first");
        ops.log.clear();
        run(&mut ops, &p).unwrap();
        assert_eq!(ops.state(5), SlotState::Absent);
        assert_eq!(ops.state(3), SlotState::Active);
        let i_left = ops.log.iter().position(|x| x == "left:5").unwrap();
        let i_stop = ops.log.iter().position(|x| x == "stop:5").unwrap();
        assert!(i_left < i_stop, "leaves the active set before stopping");
    }

    #[test]
    fn bounds_and_validator_one() {
        let ops = Mock::new(2);
        let m = manifest_from(&ops);
        assert!(plan(&m, 1, false).is_err());
        assert!(plan(&m, 11, false).is_err());
        assert!(plan(&m, 2, false).is_err());
        let mut ops = Mock::new(3);
        let p = plan(&manifest_from(&ops), 2, false).unwrap();
        assert_eq!(p.slots, vec![3]);
        assert!(shrink_step(&mut ops, 1).is_err());
    }

    #[test]
    fn interruption_resumes_and_undo_reverts() {
        let mut ops = Mock::new(2);
        ops.fail_once = Some(("bond", 3));
        let p = plan(&manifest_from(&ops), 4, false).unwrap();
        assert!(run(&mut ops, &p).is_err());
        assert_eq!(ops.state(3), SlotState::Joining);
        // Resume (Retry): picks up at the bond.
        run(&mut ops, &p).unwrap();
        assert_eq!(ops.state(4), SlotState::Active);

        // A failure mid-grow, then Undo.
        let mut ops = Mock::new(2);
        ops.fail_once = Some(("start", 4));
        let p = plan(&manifest_from(&ops), 4, false).unwrap();
        assert!(run(&mut ops, &p).is_err());
        assert_eq!((ops.state(3), ops.state(4)), (SlotState::Active, SlotState::Funded));
        let u = undo_plan(&ops, &p);
        assert_eq!(u.slots, vec![4, 3]);
        run(&mut ops, &u).unwrap();
        assert_eq!((ops.state(3), ops.state(4)), (SlotState::Absent, SlotState::Absent));
        assert!(ops.log.contains(&"withdraw:3".to_string()), "the bonded one withdraws");
    }
}
