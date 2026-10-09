//! Where everything lives (spec §5.4). Every location can be overridden by an
//! environment variable, which is how the tests and the headless smoke job
//! run without touching a real home directory.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    /// `~/Library/Application Support/F1R3FLY/ign1t10n`, mode 0700.
    pub state: PathBuf,
    /// `~/Library/Logs/F1R3FLY/ign1t10n`.
    pub logs: PathBuf,
    /// F1R3Gaze's profile directory.
    pub profile: PathBuf,
    /// Bundled executables: `f1r3node`, `embers`, and this binary.
    pub node_bin: PathBuf,
    pub embers_bin: PathBuf,
    /// `Helpers/f1r3games-service` and `Helpers/f1r3games` (the CLI).
    pub games_bin: PathBuf,
    pub games_cli: PathBuf,
    /// `Resources/f1r3games`: `games.toml`, `portal/` (the shell) and
    /// `games/<id>/` (each bundled client).
    pub games_res: PathBuf,
    /// `IGN1T10N_GAZE_BIN`: a `f1r3gaze` to use instead of the installed one
    /// (development, tests).
    pub gaze_override: Option<PathBuf>,
    /// The copy of F1R3Gaze.app ign1t10n carries in `Contents/Resources`, to
    /// install at first launch when F1R3Gaze is missing.
    pub bundled_gaze: PathBuf,
    pub self_bin: PathBuf,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl Paths {
    pub fn from_env() -> Paths {
        let h = home();
        let mac = cfg!(target_os = "macos");
        let state = env_path("IGN1T10N_STATE_DIR").unwrap_or_else(|| {
            if mac {
                h.join("Library/Application Support/F1R3FLY/ign1t10n")
            } else {
                env_path("XDG_DATA_HOME").unwrap_or_else(|| h.join(".local/share")).join("ign1t10n")
            }
        });
        let logs = env_path("IGN1T10N_LOG_DIR").unwrap_or_else(|| {
            if mac { h.join("Library/Logs/F1R3FLY/ign1t10n") } else { state.join("logs") }
        });
        let profile = env_path("F1R3GAZE_PROFILE").unwrap_or_else(|| {
            if mac {
                h.join("Library/Application Support/F1R3Gaze")
            } else {
                env_path("XDG_DATA_HOME").unwrap_or_else(|| h.join(".local/share")).join("f1r3gaze")
            }
        });
        let self_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ign1t10n"));
        // Contents/MacOS/ign1t10n -> Contents/Helpers/<tool>
        let contents = self_bin.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
        let helpers = contents.join("Helpers");
        Paths {
            node_bin: env_path("IGN1T10N_NODE_BIN").unwrap_or_else(|| helpers.join("f1r3node")),
            embers_bin: env_path("IGN1T10N_EMBERS_BIN").unwrap_or_else(|| helpers.join("embers")),
            games_bin: env_path("IGN1T10N_GAMES_BIN").unwrap_or_else(|| helpers.join("f1r3games-service")),
            games_cli: env_path("IGN1T10N_GAMES_CLI").unwrap_or_else(|| helpers.join("f1r3games")),
            games_res: env_path("IGN1T10N_GAMES_DIR").unwrap_or_else(|| contents.join("Resources/f1r3games")),
            gaze_override: env_path("IGN1T10N_GAZE_BIN"),
            bundled_gaze: contents.join("Resources/F1R3Gaze.app"),
            self_bin,
            state,
            logs,
            profile,
        }
    }

    /// Where F1R3Gaze.app may be installed, in order of preference.
    pub fn gaze_app_locations() -> Vec<PathBuf> {
        vec![PathBuf::from("/Applications/F1R3Gaze.app"), home().join("Applications/F1R3Gaze.app")]
    }

    /// The `f1r3gaze` executable: the override, else the first installed
    /// F1R3Gaze.app, else (not installed) the /Applications location.
    pub fn gaze_bin(&self) -> PathBuf {
        if let Some(o) = &self.gaze_override {
            return o.clone();
        }
        let bins = self.gaze_candidates();
        bins.iter().find(|b| b.exists()).cloned().unwrap_or_else(|| bins[0].clone())
    }

    /// Every place `gaze_bin` looks, in order: the installed apps, then, for
    /// a development build (not inside an app bundle), `f1r3gaze` on PATH.
    pub fn gaze_candidates(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = Self::gaze_app_locations().into_iter().map(|a| a.join("Contents/MacOS/f1r3gaze")).collect();
        if !self.in_app_bundle() {
            if let Some(path) = std::env::var_os("PATH") {
                v.extend(std::env::split_paths(&path).map(|d| d.join("f1r3gaze")));
            }
        }
        v
    }

    /// Is this binary `X.app/Contents/MacOS/ign1t10n`?
    pub fn in_app_bundle(&self) -> bool {
        self.self_bin.parent().is_some_and(|d| d.ends_with("Contents/MacOS"))
    }

    pub fn manifest(&self) -> PathBuf { self.state.join("shard.toml") }
    pub fn keys(&self) -> PathBuf { self.state.join("keys") }
    pub fn key(&self, name: &str) -> PathBuf { self.keys().join(format!("{name}.pem")) }
    pub fn genesis(&self) -> PathBuf { self.state.join("genesis") }
    pub fn conf(&self) -> PathBuf { self.state.join("conf") }
    pub fn conf_file(&self, name: &str) -> PathBuf { self.conf().join(format!("{name}.conf")) }
    pub fn nodes(&self) -> PathBuf { self.state.join("nodes") }
    pub fn node_dir(&self, name: &str) -> PathBuf { self.nodes().join(name) }
    pub fn run(&self) -> PathBuf { self.state.join("run") }
    pub fn socket(&self) -> PathBuf { self.run().join("control.sock") }
    pub fn pidfile(&self) -> PathBuf { self.run().join("supervisor.pid") }
    pub fn archive(&self) -> PathBuf { self.state.join("archive") }
    /// F1R3Games' state: its configuration, manifests and the CLI's home.
    pub fn games(&self) -> PathBuf { self.state.join("games") }
    pub fn games_conf(&self) -> PathBuf { self.games().join("f1r3games.toml") }
    /// The jobs' configuration: the portal's without `[relay]` (spec v0.5 §10.8).
    pub fn games_jobs_conf(&self) -> PathBuf { self.games().join("f1r3games-jobs.toml") }
    pub fn games_manifests(&self) -> PathBuf { self.games().join("manifests") }
    pub fn secrets_file(&self) -> PathBuf { self.state.join("secrets.dev.json") }
    pub fn log_file(&self, name: &str) -> PathBuf { self.logs.join(format!("{name}.log")) }
    pub fn stdout_log(&self, name: &str) -> PathBuf { self.logs.join(format!("{name}.stdout.log")) }

    /// Create the state tree with private permissions.
    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [&self.state, &self.keys(), &self.genesis(), &self.conf(), &self.nodes(), &self.run(), &self.logs] {
            std::fs::create_dir_all(d)?;
        }
        for d in [&self.state, &self.keys(), &self.run()] {
            set_mode(d, 0o700)?;
        }
        Ok(())
    }
}

pub fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
}

/// Write a file atomically (temporary file, fsync, rename) with `mode`.
pub fn write_atomic(p: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = p.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp{}", p.file_name().and_then(|n| n.to_str()).unwrap_or("f"), std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    set_mode(&tmp, mode)?;
    std::fs::rename(&tmp, p)
}
