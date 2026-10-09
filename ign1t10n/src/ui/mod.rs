//! The user interface's model: what the menu shows, what the configuration
//! panel estimates. Portable and tested; `appkit` renders it on macOS.

#[cfg(target_os = "macos")]
pub mod appkit;

use crate::control::{GamesState, Report, ShardState};
use crate::provision::{disk_needed, memory_needed_with};

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
    /// Open the F1R3Games portal in the default browser.
    OpenGames,
    /// Open a page of the portal (a game's launch page or gallery).
    OpenGamesAt(String),
    /// Apply a waiting F1R3Games update (after consent).
    UpdateGames,
    /// Start F1R3Games again after it failed (re-runs its stages).
    RetryGames,
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
            if let Some(g) = &r.games {
                let n = g.games.iter().filter(|x| x.registered && x.client).count();
                v.push(Entry::Info(format!("F1R3Games: {}, {n} game{}", g.state.label(), if n == 1 { "" } else { "s" })));
                v.push(Entry::Info(format!("   portal {}", g.url)));
                for x in g.games.iter().filter(|x| x.client) {
                    v.push(Entry::Info(format!("   {} {}", x.id, x.origin.clone().unwrap_or_default())));
                }
                if g.relay.on && !g.relay.running {
                    v.push(Entry::Info("   relay: waiting for F1R3Ink".into()));
                } else if g.relay.running {
                    v.push(Entry::Info(format!("   relay: {}", if g.relay.named { "running" } else { "running, not yet named" })));
                }
                if g.pending_update.is_some() {
                    v.push(Entry::Item("Update F1R3Games…".into(), Action::UpdateGames, true));
                }
                if matches!(g.state, GamesState::Failed(_)) {
                    v.push(Entry::Item("Try F1R3Games again".into(), Action::RetryGames, true));
                }
            }
        }
    }
    v.push(Entry::Separator);
    let running = r.map(|r| !matches!(r.shard, ShardState::Stopped | ShardState::Failed(_))).unwrap_or(false);
    let games_up = r.and_then(|r| r.games.as_ref()).map(|g| matches!(g.state, GamesState::Running | GamesState::Degraded(_))).unwrap_or(false);
    if let Some(g) = r.and_then(|r| r.games.as_ref()) {
        v.push(Entry::Item("Open F1R3Games".into(), Action::OpenGames, games_up));
        // Each registered game: start a new instance, or browse its plays
        // (spec v0.5 §12.1), in the default browser.
        for x in g.games.iter().filter(|x| x.client && x.registered) {
            let name = display_name(&x.id);
            v.push(Entry::Item(format!("   New {name} game"), Action::OpenGamesAt(format!("/games/{}/launch", x.id)), games_up));
            v.push(Entry::Item(format!("   {name} gallery"), Action::OpenGamesAt(format!("/games/{}/gallery", x.id)), games_up));
        }
    }
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

/// How a game is named to people: its id with the product's capitals.
pub fn display_name(id: &str) -> String {
    match id {
        "f1r3pix" => "F1R3Pix".into(),
        "f1r3beat" => "F1R3Beat".into(),
        "f1r3ink" => "F1R3Ink".into(),
        "f1r3sidechat" => "F1R3SideChat".into(),
        "f1r3skein" => "F1R3Skein".into(),
        other => other.into(),
    }
}

/// The portal path for `ctl games open [GAME [launch|gallery]]`.
pub fn portal_path(game: Option<&str>, page: Option<&str>) -> Result<String, String> {
    match (game, page) {
        (None, _) => Ok("/".into()),
        (Some(g), _) if !crate::games::GAME_IDS.contains(&g) => Err(format!("no game {g}; one of {}", crate::games::GAME_IDS.join(", "))),
        (Some(g), None | Some("launch")) => Ok(format!("/games/{g}/launch")),
        (Some(g), Some("gallery")) => Ok(format!("/games/{g}/gallery")),
        (Some(_), Some(p)) => Err(format!("{p}: launch or gallery")),
    }
}

pub fn endpoints(r: &Report) -> String {
    let mut s = format!("validators: {}\nobserver: {}\nshard_id: {}\n", r.gaze_validators.join(", "), r.observer, r.shard_id);
    if let Some(e) = &r.embers {
        s += &format!("embers: {e}\n");
    }
    if let Some(g) = &r.games {
        s += &format!("f1r3games: {}\n", g.url);
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
    estimate_with(n, embers, false, physical, free)
}

pub fn estimate_with(n: u8, embers: bool, games: bool, physical: Option<u64>, free: Option<u64>) -> Estimate {
    let memory = memory_needed_with(n, embers, games);
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
    fn menu_shows_f1r3games_and_opens_it_only_when_up() {
        use crate::control::{GameReport, GamesReport};
        let mut g = GamesReport { state: GamesState::Installing("x".into()), url: "http://localhost:40700".into(), ..Default::default() };
        g.games.push(GameReport { id: "f1r3pix".into(), origin: Some("http://localhost:40701".into()), client: true, registered: true, env_version: 2 });
        g.games.push(GameReport { id: "f1r3ink".into(), origin: Some("http://localhost:40703".into()), client: true, registered: false, env_version: 2 });
        let mut r = Report { shard: ShardState::Running, validators: 2, games: Some(g), ..Default::default() };
        assert!(menu(Some(&r)).contains(&Entry::Item("Open F1R3Games".into(), Action::OpenGames, false)));
        r.games.as_mut().unwrap().state = GamesState::Running;
        let m = menu(Some(&r));
        assert!(m.contains(&Entry::Item("Open F1R3Games".into(), Action::OpenGames, true)));
        assert!(m.contains(&Entry::Info("F1R3Games: Running, 1 game".into())));
        // Registered games can be launched or browsed; an unregistered one cannot.
        assert!(m.contains(&Entry::Item("   New F1R3Pix game".into(), Action::OpenGamesAt("/games/f1r3pix/launch".into()), true)));
        assert!(m.contains(&Entry::Item("   F1R3Pix gallery".into(), Action::OpenGamesAt("/games/f1r3pix/gallery".into()), true)));
        assert!(!m.iter().any(|e| matches!(e, Entry::Item(t, _, _) if t.contains("F1R3Ink"))));
        assert_eq!(portal_path(Some("f1r3ink"), Some("gallery")).unwrap(), "/games/f1r3ink/gallery");
        assert_eq!(portal_path(Some("f1r3ink"), None).unwrap(), "/games/f1r3ink/launch");
        assert_eq!(portal_path(None, None).unwrap(), "/");
        assert!(portal_path(Some("chess"), None).is_err() && portal_path(Some("f1r3ink"), Some("x")).is_err());
        r.games.as_mut().unwrap().pending_update = Some("f1r3pix".into());
        assert!(menu(Some(&r)).iter().any(|e| matches!(e, Entry::Item(_, Action::UpdateGames, true))));
        assert!(endpoints(&r).contains("f1r3games: http://localhost:40700"));
        assert!(estimate_with(2, true, true, None, None).memory > estimate(2, true, None, None).memory);
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
