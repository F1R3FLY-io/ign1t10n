//! What differs by operating system. macOS is the product; the generic
//! implementation lets the portable core run and be tested on Linux.

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod generic;
#[cfg(not(target_os = "macos"))]
pub use generic::*;

use crate::paths::Paths;
use std::path::Path;

/// Free bytes on the volume holding `p`.
pub fn free_disk(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

/// Start `ign1t10n supervise` detached from this process (headless
/// provisioning, and development off macOS).
pub fn launch_supervisor_directly(p: &Paths) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new(&p.self_bin);
    c.arg("supervise").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().map(|_| ()).map_err(|e| format!("cannot start the supervisor: {e}"))
}

/// An RAII guard; dropping it lets the Mac idle-sleep again.
pub struct Awake(#[allow(dead_code)] pub u32);
