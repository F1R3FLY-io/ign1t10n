//! The user interface's model: what the menu shows, what the configuration
//! panel estimates. Portable and tested; `appkit` renders it on macOS.

#[cfg(target_os = "macos")]
pub mod appkit;

use crate::control::{Report, ShardState};
use crate::provision::{disk_needed, memory_needed};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dot {
    None,
    Amber,
    Red,
    Hollow,
}

pub fn dot(s: &ShardState) -> Dot {
    match s {
        ShardState::Running => Dot::None,
        ShardState::Starting(_) | ShardState::Degraded(_) | ShardState::Resizing(_) => Dot::Amber,
        ShardState::Failed(_) => Dot::Red,
        ShardState::Stopped => Dot::Hollow,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    OpenGaze,
    Start,
    Stop,
    Configure,
    Fund,
    CopyEndpoints,
    ShowLogs,
    Reset,
    Uninstall,
    Quit,
    Retry,
    Undo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Info(String),
    Item(String, Action, bool),
    Separator,
}

/// The menu (spec §10.1).
pub fn menu(r: Option<&Report>) -> Vec<Entry> {
    let mut v = vec![];
    match r {
        None => v.push(Entry::Info("Local shard: supervisor not running".into())),
        Some(r) => {
            v.push(Entry::Info(format!("Local shard: {}, {} validators", r.shard.label(), r.validators)));
            let lfb = r.nodes.iter().filter(|n| n.name.starts_with("validator")).map(|n| n.lfb).max().unwrap_or(-1);
            if lfb >= 0 {
                let obs = r.nodes.iter().find(|n| n.name == "observer").map(|n| n.lfb).unwrap_or(-1);
                v.push(Entry::Info(format!("   Block {lfb}; observer {} behind", (lfb - obs).max(0))));
            }
            for n in &r.nodes {
                let mark = if n.ready { "●" } else if n.running { "◐" } else { "○" };
                v.push(Entry::Info(format!("   {mark} {} :{} {}", n.name, n.base_port, if n.name.starts_with("validator") { n.state.as_str() } else { "" })));
            }
            if let Some(rs) = &r.resize {
                v.push(Entry::Info(format!("   {rs}")));
                if rs.contains("stopped:") {
                    v.push(Entry::Item("Retry resize".into(), Action::Retry, true));
                    v.push(Entry::Item("Undo resize".into(), Action::Undo, true));
                }
            }
            if !r.exposed.is_empty() {
                v.push(Entry::Info("   ⚠ some sockets listen off loopback (node N1 pending)".into()));
            }
        }
    }
    v.push(Entry::Separator);
    let running = r.map(|r| !matches!(r.shard, ShardState::Stopped | ShardState::Failed(_))).unwrap_or(false);
    v.push(Entry::Item("Open F1R3Gaze".into(), Action::OpenGaze, true));
    v.push(if running { Entry::Item("Stop shard".into(), Action::Stop, true) } else { Entry::Item("Start shard".into(), Action::Start, true) });
    v.push(Entry::Item("Configure…".into(), Action::Configure, r.is_some()));
    v.push(Entry::Item("Fund a wallet…".into(), Action::Fund, running));
    v.push(Entry::Item("Copy endpoints".into(), Action::CopyEndpoints, r.is_some()));
    v.push(Entry::Item("Show logs".into(), Action::ShowLogs, true));
    v.push(Entry::Separator);
    v.push(Entry::Item("Reset shard…".into(), Action::Reset, r.is_some()));
    v.push(Entry::Item("Uninstall…".into(), Action::Uninstall, true));
    v.push(Entry::Separator);
    v.push(Entry::Item("Quit ign1t10n".into(), Action::Quit, true));
    v
}

pub fn endpoints(r: &Report) -> String {
    let mut s = format!("validators: {}\nobserver: {}\nshard_id: {}\n", r.gaze_validators.join(", "), r.observer, r.shard_id);
    if let Some(e) = &r.embers {
        s += &format!("embers: {e}\n");
    }
    s
}

#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    Fine,
    /// Above 50 % of physical memory.
    Warn,
    /// Above 80 %: Apply asks for confirmation.
    Confirm,
}

pub struct Estimate {
    pub memory: u64,
    pub disk: u64,
    pub load: Load,
    pub text: String,
}

pub fn estimate(n: u8, embers: bool, physical: Option<u64>, free: Option<u64>) -> Estimate {
    let memory = memory_needed(n, embers);
    let disk = disk_needed(n);
    let gb = |b: u64| format!("{:.1} GB", b as f64 / (1u64 << 30) as f64);
    let (load, of) = match physical {
        Some(p) if memory * 10 > p * 8 => (Load::Confirm, format!(" of {}", gb(p))),
        Some(p) if memory * 2 > p => (Load::Warn, format!(" of {}", gb(p))),
        Some(p) => (Load::Fine, format!(" of {}", gb(p))),
        None => (Load::Fine, String::new()),
    };
    let fr = free.map(|f| format!(" ({} free)", gb(f))).unwrap_or_default();
    Estimate { memory, disk, load, text: format!("About {} memory{of}; {} disk{fr}. (Provisional figures, to be measured.)", gb(memory), gb(disk)) }
}

/// Measured fault tolerance (Obligation "Measured fault tolerance"): how
/// many validators may stop while blocks still finalise, per N. `None` until
/// T1 has measured it; N = 2 is expected to tolerate none.
pub const TOLERANCE: [Option<u8>; 11] = [None, None, Some(0), None, None, None, None, None, None, None, None];

pub fn tolerance_text(n: u8) -> String {
    match TOLERANCE.get(n as usize).copied().flatten() {
        Some(0) => format!("With {n} validators, finality stops if any validator stops."),
        Some(k) => format!("With {n} validators, finality continues with {k} stopped."),
        None => format!("Fault tolerance with {n} validators has not been measured yet."),
    }
}

/// Run the menu-bar application.
pub fn run(first_run: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        appkit::run(first_run)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = first_run;
        Err("the menu bar is macOS only; use `ign1t10n ctl` (try `ign1t10n ctl provision --non-interactive`)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_offers_start_or_stop() {
        let mut r = Report { shard: ShardState::Stopped, validators: 2, ..Default::default() };
        assert!(menu(Some(&r)).contains(&Entry::Item("Start shard".into(), Action::Start, true)));
        r.shard = ShardState::Running;
        assert!(menu(Some(&r)).contains(&Entry::Item("Stop shard".into(), Action::Stop, true)));
        assert_eq!(dot(&ShardState::Failed("x".into())), Dot::Red);
    }

    #[test]
    fn estimates_warn_and_confirm() {
        let g = 1u64 << 30;
        assert_eq!(estimate(2, true, Some(64 * g), None).load, Load::Fine);
        assert_eq!(estimate(10, true, Some(16 * g), None).load, Load::Warn);
        assert_eq!(estimate(10, true, Some(8 * g), None).load, Load::Confirm);
        assert!(tolerance_text(2).contains("any validator stops"));
    }
}
