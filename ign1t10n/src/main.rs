//! `ign1t10n`: the menu bar (no arguments), `supervise` (the launch agent's
//! program), and `ctl` (a command-line client of the control socket).

use ign1t10n::control::{self, Request, Response};
use ign1t10n::manifest::Manifest;
use ign1t10n::paths::Paths;
use ign1t10n::provision::{self, Choices};
use std::time::Duration;

const USAGE: &str = "usage:
  ign1t10n [--first-run]                 the menu-bar application
  ign1t10n supervise                     run the shard (the launch agent's program)
  ign1t10n ctl status [--json]
  ign1t10n ctl start|stop|restart
  ign1t10n ctl resize N [--new-shard]    2 <= N <= 10
  ign1t10n ctl retry|undo                a stopped resize
  ign1t10n ctl fund ADDRESS F1R3         from the local faucet
  ign1t10n ctl embers on|off
  ign1t10n ctl gaze on|off               point F1R3Gaze at the local shard
  ign1t10n ctl reallocate NODE           new ports for a node whose ports are taken
  ign1t10n ctl env                       endpoints, for scripts
  ign1t10n ctl logs [NODE]               last lines of a log
  ign1t10n ctl provision [--non-interactive] [--validators N] [--no-embers] [--no-gaze] [--open]
  ign1t10n ctl reset [--validators N] --yes
  ign1t10n ctl uninstall --yes [--keep-archive]
  ign1t10n --version";

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("ign1t10n: {msg}");
    std::process::exit(1)
}

fn call(p: &Paths, r: Request) -> Response {
    let timeout = if matches!(r, Request::Fund { .. }) { Duration::from_secs(600) } else { Duration::from_secs(120) };
    match control::call(p, &r, timeout) {
        Ok(resp) if resp.ok => resp,
        Ok(resp) => die(resp.error.unwrap_or_else(|| "failed".into())),
        Err(e) => die(e),
    }
}

fn say(p: &Paths, r: Request) {
    let resp = call(p, r);
    if let Some(m) = resp.message {
        println!("{m}");
    }
}

fn flag(args: &[String], f: &str) -> bool {
    args.iter().any(|a| a == f)
}

fn opt<T: std::str::FromStr>(args: &[String], f: &str) -> Option<T> {
    args.windows(2).find(|w| w[0] == f).and_then(|w| w[1].parse().ok())
}

fn on_off(s: Option<&String>) -> bool {
    match s.map(String::as_str) {
        Some("on") => true,
        Some("off") => false,
        _ => die("expected on or off"),
    }
}

fn status(p: &Paths, json: bool) {
    let r = call(p, Request::Status).status.unwrap_or_default();
    if json {
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
        return;
    }
    println!("{}, {} validators", r.shard.label(), r.validators);
    if let Some(h) = &r.genesis_hash {
        println!("genesis {h}");
    }
    for n in &r.nodes {
        println!("  {:<13} :{:<5} {:<9} {:<8} lfb {:>5}  peers {}", n.name, n.base_port, n.state, if n.ready { "ready" } else if n.running { "starting" } else { "down" }, n.lfb, n.peers);
    }
    if let Some(rs) = &r.resize {
        println!("resize: {rs}");
    }
    if !r.exposed.is_empty() {
        println!("warning: listening off loopback: {}", r.exposed.join(", "));
    }
}

fn ctl(p: &Paths, args: &[String]) {
    let verb = args.first().map(String::as_str).unwrap_or("status");
    let rest = &args[args.len().min(1)..];
    match verb {
        "status" => status(p, flag(rest, "--json")),
        "start" => say(p, Request::Start),
        "stop" => say(p, Request::Stop),
        "restart" => say(p, Request::Restart),
        "resize" => {
            let n: u8 = rest.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| die("resize needs a number of validators"));
            say(p, Request::Resize { target: n, new_shard: flag(rest, "--new-shard") })
        }
        "retry" => say(p, Request::Retry),
        "undo" => say(p, Request::Undo),
        "fund" => {
            let (Some(a), Some(n)) = (rest.first(), rest.get(1).and_then(|s| s.parse().ok())) else { die("fund ADDRESS F1R3") };
            say(p, Request::Fund { address: a.clone(), f1r3: n })
        }
        "embers" => say(p, Request::SetOptions { embers: Some(on_off(rest.first())), gaze_integration: None }),
        "gaze" => say(p, Request::SetOptions { embers: None, gaze_integration: Some(on_off(rest.first())) }),
        "reallocate" => say(p, Request::Reallocate { node: rest.first().cloned().unwrap_or_else(|| die("reallocate NODE")) }),
        "env" => {
            let r = call(p, Request::Status).status.unwrap_or_default();
            println!("F1R3_VALIDATORS={}", r.gaze_validators.join(","));
            println!("F1R3_OBSERVER={}", r.observer);
            println!("F1R3_SHARD_ID={}", r.shard_id);
            if let Some(e) = r.embers {
                println!("F1R3_EMBERS={e}");
            }
        }
        "logs" => {
            let f = match rest.first() {
                Some(n) => p.stdout_log(n),
                None => p.log_file("ign1t10n"),
            };
            let t = std::fs::read_to_string(&f).unwrap_or_else(|e| die(format!("{}: {e}", f.display())));
            let lines: Vec<&str> = t.lines().collect();
            for l in &lines[lines.len().saturating_sub(60)..] {
                println!("{l}");
            }
            eprintln!("({})", f.display());
        }
        "provision" => {
            let d = Choices::default();
            let c = Choices {
                validators: opt(rest, "--validators").unwrap_or(d.validators),
                embers: !flag(rest, "--no-embers"),
                gaze_integration: !flag(rest, "--no-gaze"),
            };
            if !(ign1t10n::MIN_VALIDATORS..=ign1t10n::MAX_VALIDATORS).contains(&c.validators) {
                die("--validators must be between 2 and 10");
            }
            let secrets = ign1t10n::secrets::open(p);
            let run = provision::Run { paths: p, secrets: &*secrets, choices: c, progress: &provision::Quiet, headless: flag(rest, "--non-interactive"), open: flag(rest, "--open") };
            match provision::provision(&run) {
                Ok(m) => println!("local shard {} ready: {} validators, genesis {}", m.shard.network_id, m.n(), m.shard.genesis_hash.unwrap_or_default()),
                Err(e) => die(e),
            }
        }
        "reset" => {
            if !flag(rest, "--yes") {
                die("reset erases every deploy, registry entry and balance on the local shard (F1R3Gaze wallets are kept and funded again); add --yes");
            }
            let n = opt(rest, "--validators").unwrap_or_else(|| Manifest::load(p).ok().flatten().map(|m| m.n()).unwrap_or(2));
            say(p, Request::Resize { target: n, new_shard: true })
        }
        "uninstall" => {
            if !flag(rest, "--yes") {
                die("uninstall removes the local shard, its logs and keys (F1R3Gaze and its wallets are left alone); add --yes");
            }
            let _ = control::call(p, &Request::Shutdown, Duration::from_secs(10));
            for _ in 0..300 {
                if !p.socket().exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if let Err(e) = ign1t10n::platform::unregister_agent() {
                eprintln!("warning: {e}");
            }
            let secrets = ign1t10n::secrets::open(p);
            ign1t10n::lifecycle::uninstall_files(p, &*secrets, flag(rest, "--keep-archive")).unwrap_or_else(|e| die(e));
            println!("the local shard is uninstalled; F1R3Gaze and its wallets are untouched. Drag ign1t10n.app to the Trash to finish (or run its Resources/uninstall.sh).");
        }
        _ => die(USAGE),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let p = Paths::from_env();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => println!("ign1t10n {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h") => println!("{USAGE}"),
        Some("supervise") => {
            if let Err(e) = ign1t10n::supervisor::Supervisor::new(p).run() {
                ign1t10n::error!("{e}");
                // Non-zero: launchd restarts us (KeepAlive SuccessfulExit = false).
                die(e);
            }
        }
        Some("ctl") => ctl(&p, &args[1..]),
        None | Some("--first-run") | Some("-psn_0_0") => {
            ign1t10n::logging::init(&p.log_file("ign1t10n"));
            if let Err(e) = ign1t10n::ui::run(args.first().map(|a| a == "--first-run").unwrap_or(false)) {
                die(e);
            }
        }
        Some(_) => die(USAGE),
    }
}
