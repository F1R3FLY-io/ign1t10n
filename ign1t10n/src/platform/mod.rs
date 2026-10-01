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

/// Copy an application bundle to the first writable location in `to`
/// (`/Applications`, then `~/Applications`), preserving signatures and
/// extended attributes (`ditto`), and return where it went.
pub fn install_app(app: &Path, to: &[std::path::PathBuf]) -> Result<std::path::PathBuf, String> {
    let mut last = String::from("no destination");
    for dst in to {
        let Some(parent) = dst.parent() else { continue };
        if std::fs::create_dir_all(parent).is_err() {
            continue;
        }
        let tmp = parent.join(format!(".{}.ign1t10n-{}", dst.file_name().unwrap().to_string_lossy(), std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let copy = if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/ditto").arg(app).arg(&tmp).status()
        } else {
            std::process::Command::new("cp").arg("-a").arg(app).arg(&tmp).status()
        };
        match copy {
            Ok(s) if s.success() => match std::fs::rename(&tmp, dst) {
                Ok(()) => {
                    #[cfg(target_os = "macos")]
                    let _ = std::process::Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister").arg("-f").arg(dst).status();
                    return Ok(dst.clone());
                }
                Err(e) => last = format!("{}: {e}", dst.display()),
            },
            Ok(_) | Err(_) => last = format!("could not copy F1R3Gaze to {}", parent.display()),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
    Err(last)
}

/// An RAII guard; dropping it lets the Mac idle-sleep again.
pub struct Awake(#[allow(dead_code)] pub u32);

#[cfg(test)]
mod tests {
    #[test]
    fn install_app_falls_back_to_a_writable_location() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("src/F1R3Gaze.app/Contents/MacOS");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("f1r3gaze"), "#!/bin/sh\n").unwrap();
        // The first location cannot be created (its parent is a file).
        std::fs::write(d.path().join("not-a-dir"), "").unwrap();
        let to = [d.path().join("not-a-dir/F1R3Gaze.app"), d.path().join("home/Applications/F1R3Gaze.app")];
        let got = super::install_app(&d.path().join("src/F1R3Gaze.app"), &to).unwrap();
        assert_eq!(got, to[1]);
        assert!(to[1].join("Contents/MacOS/f1r3gaze").exists());
    }
}
