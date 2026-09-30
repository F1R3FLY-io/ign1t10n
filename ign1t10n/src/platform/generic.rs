//! Off macOS: no launch agent (the supervisor is launched directly), no
//! Keychain (see `secrets::FileSecrets`), notifications to the log.

use super::Awake;

pub fn macos_version() -> Option<(u32, u32)> {
    None
}

pub fn physical_memory() -> Option<u64> {
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
}

pub fn check_gaze_signature() -> Result<(), String> {
    Ok(())
}

pub fn register_agent(_waiting: &dyn Fn(&str)) -> Result<(), String> {
    super::launch_supervisor_directly(&crate::paths::Paths::from_env())
}

pub fn unregister_agent() -> Result<(), String> {
    Ok(())
}

pub fn agent_enabled() -> Option<bool> {
    None
}

pub fn open_gaze() -> Result<(), String> {
    let p = crate::paths::Paths::from_env();
    std::process::Command::new(&p.gaze_bin).arg("gaze://newtab").spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn reveal(path: &std::path::Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

pub fn notify(title: &str, body: &str) {
    crate::info!("notification: {title}: {body}");
}

pub fn prevent_idle_sleep(_why: &str) -> Option<Awake> {
    None
}

pub fn copy_to_clipboard(_s: &str) {}
