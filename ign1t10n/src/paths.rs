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
    pub gaze_bin: PathBuf,
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
        let helpers = self_bin.parent().and_then(Path::parent).map(|c| c.join("Helpers")).unwrap_or_default();
        Paths {
            node_bin: env_path("IGN1T10N_NODE_BIN").unwrap_or_else(|| helpers.join("f1r3node")),
            embers_bin: env_path("IGN1T10N_EMBERS_BIN").unwrap_or_else(|| helpers.join("embers")),
            gaze_bin: env_path("IGN1T10N_GAZE_BIN")
                .unwrap_or_else(|| PathBuf::from("/Applications/F1R3Gaze.app/Contents/MacOS/f1r3gaze")),
            self_bin,
            state,
            logs,
            profile,
        }
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
