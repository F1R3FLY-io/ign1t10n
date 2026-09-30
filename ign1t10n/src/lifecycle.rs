//! Upgrade compatibility, archiving, reset and uninstall (spec §12).

use crate::manifest::Manifest;
use crate::paths::Paths;
use crate::secrets::Secrets;
use std::path::Path;

const COMPAT: &str = include_str!("../compat.toml");

#[derive(serde::Deserialize)]
struct Compat {
    current: u32,
    readable: Vec<u32>,
}

/// Refuse to start a node binary against a data directory it cannot read.
pub fn check_compat(m: &Manifest) -> Result<(), String> {
    let c: Compat = toml::from_str(COMPAT).map_err(|e| format!("compat.toml: {e}"))?;
    match m.shard.storage_format {
        Some(f) if !c.readable.contains(&f) => Err(format!(
            "this shard's data (storage format {f}) cannot be opened by the installed node (format {}); start a new shard to continue",
            c.current
        )),
        _ => Ok(()),
    }
}

fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn move_into(src: &Path, dir: &Path) -> Result<(), String> {
    if !src.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dst = dir.join(src.file_name().unwrap());
    std::fs::rename(src, &dst).map_err(|e| format!("{} -> {}: {e}", src.display(), dst.display()))
}

/// A validator slot that has left: its data directory, configuration and
/// PEM go to `archive/<timestamp>/<name>/`; its Keychain item is deleted.
pub fn archive_slot(p: &Paths, s: &dyn Secrets, name: &str) -> Result<(), String> {
    let dir = p.archive().join(stamp()).join(name);
    move_into(&p.node_dir(name), &dir)?;
    move_into(&p.conf_file(name), &dir)?;
    move_into(&p.key(name), &dir)?;
    s.delete(&crate::secrets::pem_account(name))
}

/// Archive the whole shard (reset, new shard, incompatible upgrade). The
/// data directories are node identities; they are moved, never deleted.
pub fn archive_shard(p: &Paths, s: &dyn Secrets, m: &Manifest) -> Result<std::path::PathBuf, String> {
    let dir = p.archive().join(stamp());
    for d in [p.nodes(), p.genesis(), p.conf(), p.keys()] {
        move_into(&d, &dir)?;
    }
    let _ = m.save_to(&dir.join("shard.toml"));
    for a in s.accounts()? {
        if a.starts_with("pem.") || a == crate::embers::ACCOUNT {
            s.delete(&a)?;
        }
    }
    p.ensure().map_err(|e| e.to_string())?;
    Ok(dir)
}

impl Manifest {
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        let t = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::paths::write_atomic(path, t.as_bytes(), 0o600).map_err(|e| e.to_string())
    }
}

/// Uninstall (spec §12.3), after the supervisor has stopped the shard and
/// the agent is unregistered: remove the state (optionally keeping
/// `archive/`), the logs, every Keychain item and F1R3Gaze's managed
/// settings. F1R3Gaze, its profile and its wallets are left alone.
pub fn uninstall_files(p: &Paths, s: &dyn Secrets, keep_archive: bool) -> Result<(), String> {
    crate::gaze::remove(&p.profile)?;
    for a in s.accounts().unwrap_or_default() {
        let _ = s.delete(&a);
    }
    if keep_archive && p.archive().exists() {
        for e in std::fs::read_dir(&p.state).map_err(|e| e.to_string())?.flatten() {
            if e.path() != p.archive() {
                let _ = if e.path().is_dir() { std::fs::remove_dir_all(e.path()) } else { std::fs::remove_file(e.path()) };
            }
        }
    } else {
        let _ = std::fs::remove_dir_all(&p.state);
    }
    let _ = std::fs::remove_dir_all(&p.logs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_refuses_unknown_formats() {
        let mut m = crate::manifest::tests_support::sample();
        assert!(check_compat(&m).is_ok());
        m.shard.storage_format = Some(1);
        assert!(check_compat(&m).is_ok());
        m.shard.storage_format = Some(99);
        assert!(check_compat(&m).is_err());
    }

    #[test]
    fn archive_moves_and_forgets_secrets() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Paths::from_env();
        p.state = d.path().join("state");
        p.ensure().unwrap();
        let s = crate::secrets::FileSecrets { path: p.secrets_file() };
        s.set("pem.validator-3", "pw").unwrap();
        std::fs::create_dir_all(p.node_dir("validator-3")).unwrap();
        std::fs::write(p.key("validator-3"), "k").unwrap();
        archive_slot(&p, &s, "validator-3").unwrap();
        assert!(!p.node_dir("validator-3").exists());
        assert!(s.get("pem.validator-3").unwrap().is_none());
        let a = std::fs::read_dir(p.archive()).unwrap().next().unwrap().unwrap().path();
        assert!(a.join("validator-3/validator-3.pem").exists());
    }
}
