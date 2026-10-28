//! `ctl drive` subcommands: add, up, down, remove, status, track.
//!
//! A drive is a named f1r3drive FUSE mount that reads blobs from a
//! chosen sidecar and routes writes through a chosen validator. State
//! lives per drive under `state/drives/<alias>/`:
//!
//!   deployer.hex   32-byte secp256k1 secret, raw hex (f1r3drive's `--key-file`)
//!   .f1r3drive/    f1r3drive's own state dir (identity, blob key store)
//!
//! The mount directory sits outside the shard state tree so Finder can
//! open it without descending into the installer's private 0700 state;
//! each drive mounts at `~/f1r3drive/<alias>/` by default.

use crate::control::DriveReport;
use crate::manifest::DriveConf;
use crate::supervisor::Supervisor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where `~/f1r3drive/<alias>` lives.
fn default_mount_dir(alias: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    home.join("f1r3drive").join(alias)
}

/// Create the per-drive state tree with private perms and generate the
/// deployer key.
fn ensure_state(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    crate::paths::set_mode(dir, 0o700).map_err(|e| format!("{}: {e}", dir.display()))?;
    let identity = dir.join(".f1r3drive");
    std::fs::create_dir_all(&identity).map_err(|e| format!("{}: {e}", identity.display()))?;
    crate::paths::set_mode(&identity, 0o700).ok();
    Ok(())
}

fn write_key(path: &Path) -> Result<(), String> {
    let k = crate::keys::Key::generate();
    crate::paths::write_atomic(path, k.secret_hex().as_bytes(), 0o600).map_err(|e| format!("{}: {e}", path.display()))
}

fn validate_alias(alias: &str) -> Result<(), String> {
    if alias.is_empty() || alias.len() > 64 {
        return Err("drive alias must be 1..=64 characters".into());
    }
    if !alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("drive alias may contain only letters, digits, '-' and '_'".into());
    }
    Ok(())
}

pub fn add(sup: &Supervisor, alias: &str) -> Result<(), String> {
    validate_alias(alias)?;
    let m = sup.manifest_public()?;
    if m.drives.contains_key(alias) {
        return Err(format!("drive {alias} already exists"));
    }
    // Pick the first active validator as the write path and the observer
    // as the read path. The user can edit shard.toml later to re-pin.
    let validator = m
        .validators
        .iter()
        .find(|v| v.state == crate::manifest::SlotState::Active)
        .map(|v| crate::nodeconf::validator_name(v.slot))
        .ok_or("no active validator to pin the drive to")?;
    let sidecar = if m.sidecar_configs.contains_key(&validator) {
        validator.clone()
    } else {
        m.sidecar_configs
            .keys()
            .next()
            .cloned()
            .ok_or("no sidecar configured; run `ctl provision` to finish setup")?
    };
    let state_dir = sup.paths.drive_dir(alias);
    ensure_state(&state_dir)?;
    let key_file = state_dir.join("deployer.hex");
    write_key(&key_file)?;
    let mount_dir = default_mount_dir(alias);
    std::fs::create_dir_all(&mount_dir).map_err(|e| format!("{}: {e}", mount_dir.display()))?;
    let conf = DriveConf {
        state_dir: state_dir.join(".f1r3drive").display().to_string(),
        key_file: key_file.display().to_string(),
        mount_dir: mount_dir.display().to_string(),
        validator,
        observer: "observer".into(),
        sidecar,
        tracked_pubkeys: vec![],
        pid: None,
    };
    sup.update_public(|m| {
        m.drives.insert(alias.to_string(), conf.clone());
        Ok(())
    })?;
    Ok(())
}

pub fn up(sup: &Supervisor, alias: &str) -> Result<(), String> {
    let m = sup.manifest_public()?;
    let d = m.drives.get(alias).ok_or_else(|| format!("no drive {alias}"))?.clone();
    std::fs::create_dir_all(&d.mount_dir).map_err(|e| format!("{}: {e}", d.mount_dir))?;
    sup.start_drive(alias)?;
    // Poll `mount` for the FUSE entry so callers can `std::fs::write`
    // immediately after this returns. In tests (IGN1T10N_FAKE_MOUNTS=1)
    // the fake daemon drops a stub file instead of mounting; look for
    // that marker as proof of liveness.
    let fake = std::env::var_os("IGN1T10N_FAKE_MOUNTS").is_some();
    let timeout = Duration::from_secs(60);
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        let ready = if fake {
            Path::new(&d.mount_dir).join(".f1r3drive-fake-mount").exists()
        } else {
            is_mounted(&d.mount_dir)
        };
        if ready {
            if let Some(pid) = sup.drive_pid(alias) {
                sup.update_public(|m| {
                    if let Some(x) = m.drives.get_mut(alias) {
                        x.pid = Some(pid as i32);
                    }
                    Ok(())
                })?;
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!("drive {alias} did not mount at {} within {}s", d.mount_dir, timeout.as_secs()))
}

pub fn down(sup: &Supervisor, alias: &str) -> Result<(), String> {
    let m = sup.manifest_public()?;
    let d = m.drives.get(alias).ok_or_else(|| format!("no drive {alias}"))?.clone();
    sup.stop_drive(alias);
    // Belt and braces: a crashed f1r3drive sometimes leaves the FUSE
    // mount in place. Attempt an unmount; ignore the error if it is
    // already gone.
    if is_mounted(&d.mount_dir) {
        let _ = std::process::Command::new("umount").arg(&d.mount_dir).status();
    }
    sup.update_public(|m| {
        if let Some(x) = m.drives.get_mut(alias) {
            x.pid = None;
        }
        Ok(())
    })?;
    Ok(())
}

pub fn remove(sup: &Supervisor, alias: &str) -> Result<(), String> {
    down(sup, alias)?;
    let m = sup.manifest_public()?;
    let d = m.drives.get(alias).ok_or_else(|| format!("no drive {alias}"))?.clone();
    let state_dir = sup.paths.drive_dir(alias);
    let _ = std::fs::remove_dir_all(&state_dir);
    let _ = std::fs::remove_dir(&d.mount_dir);
    sup.update_public(|m| {
        m.drives.remove(alias);
        Ok(())
    })?;
    Ok(())
}

pub fn status(sup: &Supervisor) -> Vec<DriveReport> {
    let m = match sup.manifest_public() {
        Ok(m) => m,
        Err(_) => return vec![],
    };
    m.drives
        .iter()
        .map(|(alias, d)| {
            let running = sup.running_role(&crate::procs::Role::F1r3Drive { alias: alias.clone() });
            DriveReport {
                alias: alias.clone(),
                mount_dir: d.mount_dir.clone(),
                validator: d.validator.clone(),
                observer: d.observer.clone(),
                sidecar: d.sidecar.clone(),
                running,
                pid: d.pid,
                tracked_pubkeys: d.tracked_pubkeys.clone(),
            }
        })
        .collect()
}

pub fn repoint(
    sup: &Supervisor,
    alias: &str,
    validator: Option<&str>,
    observer: Option<&str>,
    sidecar: Option<&str>,
) -> Result<(), String> {
    if validator.is_none() && observer.is_none() && sidecar.is_none() {
        return Err("repoint needs at least one of --validator, --observer, --sidecar".into());
    }
    let m = sup.manifest_public()?;
    // Validate every requested change against the current manifest
    // before we touch anything, so a partial repoint is impossible.
    if let Some(v) = validator {
        if !m.validators.iter().any(|x| crate::nodeconf::validator_name(x.slot) == v) {
            return Err(format!("unknown validator {v}"));
        }
    }
    if let Some(o) = observer {
        let known = o == "observer" || o == "bootstrap" || m.validators.iter().any(|x| crate::nodeconf::validator_name(x.slot) == o);
        if !known {
            return Err(format!("unknown observer target {o}"));
        }
    }
    if let Some(s) = sidecar {
        if !m.sidecar_configs.contains_key(s) {
            return Err(format!("no sidecar configured for {s}"));
        }
    }
    let was_running = sup.running_role(&crate::procs::Role::F1r3Drive { alias: alias.into() });
    sup.update_public(|m| {
        let d = m.drives.get_mut(alias).ok_or_else(|| format!("no drive {alias}"))?;
        if let Some(v) = validator {
            d.validator = v.into();
        }
        if let Some(o) = observer {
            d.observer = o.into();
        }
        if let Some(s) = sidecar {
            d.sidecar = s.into();
        }
        Ok(())
    })?;
    if was_running {
        down(sup, alias)?;
        up(sup, alias)?;
    }
    Ok(())
}

pub fn track(sup: &Supervisor, alias: &str, peer: &str) -> Result<(), String> {
    if !peer.chars().all(|c| c.is_ascii_hexdigit()) || peer.is_empty() {
        return Err("tracked pubkey must be hex".into());
    }
    let was_running = sup.running_role(&crate::procs::Role::F1r3Drive { alias: alias.into() });
    sup.update_public(|m| {
        let d = m.drives.get_mut(alias).ok_or_else(|| format!("no drive {alias}"))?;
        if !d.tracked_pubkeys.iter().any(|p| p == peer) {
            d.tracked_pubkeys.push(peer.into());
        }
        Ok(())
    })?;
    if was_running {
        down(sup, alias)?;
        up(sup, alias)?;
    }
    Ok(())
}

/// Is `path` the mount point of any live filesystem? Reads `/proc/mounts`
/// on Linux and shells out to `/sbin/mount` on macOS; a parse error means
/// "not mounted".
pub fn is_mounted(path: &str) -> bool {
    let want = Path::new(path);
    #[cfg(target_os = "linux")]
    {
        if let Ok(t) = std::fs::read_to_string("/proc/mounts") {
            return t.lines().any(|l| l.split_whitespace().nth(1).map(Path::new) == Some(want));
        }
    }
    if let Ok(out) = std::process::Command::new("/sbin/mount").output() {
        let s = String::from_utf8_lossy(&out.stdout);
        return s.lines().any(|l| {
            // Lines look like "f1r3drive on /path (osxfuse, nodev, nosuid)".
            l.split(" on ")
                .nth(1)
                .and_then(|rest| rest.rsplitn(2, " (").last())
                .map(Path::new)
                == Some(want)
        });
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_validated() {
        assert!(validate_alias("alice").is_ok());
        assert!(validate_alias("alice-01").is_ok());
        assert!(validate_alias("").is_err());
        assert!(validate_alias("has space").is_err());
        assert!(validate_alias("../etc").is_err());
    }
}
