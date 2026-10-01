//! macOS services: Keychain, SMAppService, IOPMAssertion, Launch Services.
//!
//! NOT COMPILED IN THE AUTHORING ENVIRONMENT (Linux). Built and exercised by
//! the `macos` CI job; see docs/IMPLEMENTATION.md.

use super::Awake;
use crate::secrets::{SERVICE, Secrets};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, msg_send_id};
use objc2_foundation::NSString;
use std::process::Command;

// ---------------------------------------------------------------------------
// Keychain: generic passwords under service io.f1r3fly.ign1t10n. Items are
// created by this binary, so its designated requirement is on their ACL and
// the menu bar, supervisor and ctl read them without a prompt.

pub struct Keychain;

impl Secrets for Keychain {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match security_framework::passwords::get_generic_password(SERVICE, account) {
            Ok(b) => String::from_utf8(b).map(Some).map_err(|e| e.to_string()),
            Err(e) if e.code() == -25300 => Ok(None), // errSecItemNotFound
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        security_framework::passwords::set_generic_password(SERVICE, account, value.as_bytes()).map_err(|e| format!("Keychain: {e}"))
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        match security_framework::passwords::delete_generic_password(SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }
    fn accounts(&self) -> Result<Vec<String>, String> {
        use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
        let found = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(SERVICE)
            .load_attributes(true)
            .limit(Limit::All)
            .search();
        match found {
            Ok(items) => Ok(items.iter().filter_map(|r| r.simplify_dict()).filter_map(|d| d.get("acct").cloned()).collect()),
            Err(e) if e.code() == -25300 => Ok(vec![]),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// System facts.

pub fn macos_version() -> Option<(u32, u32)> {
    let out = Command::new("/usr/bin/sw_vers").arg("-productVersion").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.trim().split('.').map(|x| x.parse::<u32>().unwrap_or(0));
    Some((it.next()?, it.next().unwrap_or(0)))
}

pub fn physical_memory() -> Option<u64> {
    let mut v: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = std::ffi::CString::new("hw.memsize").unwrap();
    let r = unsafe { libc::sysctlbyname(name.as_ptr(), &mut v as *mut u64 as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
    (r == 0).then_some(v)
}

fn team_id(app: &str) -> Option<String> {
    let out = Command::new("/usr/bin/codesign").args(["-dv", "--verbose=2", app]).output().ok()?;
    String::from_utf8_lossy(&out.stderr).lines().find_map(|l| l.strip_prefix("TeamIdentifier=")).map(str::to_string).filter(|t| t != "not set")
}

/// The `.app` bundle containing the configured `f1r3gaze` executable
/// (`.../F1R3Gaze.app/Contents/MacOS/f1r3gaze`), if it is inside one.
fn gaze_bundle() -> Option<std::path::PathBuf> {
    let bin = crate::paths::Paths::from_env().gaze_bin();
    let app = bin.ancestors().nth(3)?.to_path_buf();
    (app.extension().is_some_and(|e| e == "app") && bin.parent()?.ends_with("Contents/MacOS")).then_some(app)
}

fn bundle_id(app: &std::path::Path) -> Option<String> {
    let out = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleIdentifier", &app.join("Contents/Info.plist").display().to_string()])
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// S0: the F1R3Gaze bundle ign1t10n will drive has F1R3Gaze's identifier and
/// a valid signature from the same team as ign1t10n. The signature check is
/// skipped for unsigned development builds of ign1t10n; the whole check is
/// skipped when `f1r3gaze` is not inside an app bundle (a development or
/// test stand-in named by IGN1T10N_GAZE_BIN).
pub fn check_gaze_signature() -> Result<(), String> {
    let Some(app) = gaze_bundle() else { return Ok(()) };
    let gaze = app.display().to_string();
    match bundle_id(&app) {
        Some(id) if id == crate::GAZE_BUNDLE_ID => {}
        Some(id) => return Err(format!("{gaze} has bundle identifier {id}, expected {}", crate::GAZE_BUNDLE_ID)),
        None => return Err(format!("cannot read {gaze}/Contents/Info.plist")),
    }
    let own = std::env::current_exe().ok().and_then(|p| p.ancestors().nth(3).map(|a| a.display().to_string()));
    let Some(ours) = own.as_deref().filter(|a| a.ends_with(".app")).and_then(team_id) else { return Ok(()) };
    let ok = Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict", &gaze]).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return Err("F1R3Gaze's signature does not verify".into());
    }
    if team_id(&gaze).as_deref() != Some(ours.as_str()) {
        return Err("F1R3Gaze is not signed by F1R3FLY.io's team".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SMAppService (ServiceManagement, macOS 13+).

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

const SM_NOT_REGISTERED: isize = 0;
const SM_ENABLED: isize = 1;
const SM_REQUIRES_APPROVAL: isize = 2;

fn agent_service() -> Retained<AnyObject> {
    let name = NSString::from_str(crate::AGENT_PLIST);
    unsafe { msg_send_id![class!(SMAppService), agentServiceWithPlistName: &*name] }
}

fn status() -> isize {
    let svc = agent_service();
    unsafe { msg_send![&*svc, status] }
}

pub fn agent_enabled() -> Option<bool> {
    Some(status() == SM_ENABLED)
}

pub fn open_login_items() {
    unsafe {
        let _: () = msg_send![class!(SMAppService), openSystemSettingsLoginItems];
    }
}

/// S6: register the launch agent; if macOS asks for approval, explain,
/// open the Login Items settings, and wait for the person to allow it.
pub fn register_agent(waiting: &dyn Fn(&str)) -> Result<(), String> {
    let svc = agent_service();
    if status() != SM_ENABLED {
        let mut err: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = unsafe { msg_send![&*svc, registerAndReturnError: &mut err] };
        if !ok && status() != SM_REQUIRES_APPROVAL {
            let desc = if err.is_null() {
                "unknown error".to_string()
            } else {
                let d: Retained<NSString> = unsafe { msg_send_id![&*err, localizedDescription] };
                d.to_string()
            };
            return Err(format!("could not register the background item: {desc}"));
        }
    }
    let t0 = std::time::Instant::now();
    let mut asked = false;
    loop {
        match status() {
            SM_ENABLED => return Ok(()),
            SM_REQUIRES_APPROVAL => {
                if !asked {
                    waiting("macOS asks you to allow ign1t10n in System Settings › General › Login Items. Turn it on to start the shard.");
                    open_login_items();
                    asked = true;
                }
            }
            SM_NOT_REGISTERED if t0.elapsed().as_secs() > 5 => return Err("the background item was not registered".into()),
            _ => {}
        }
        if t0.elapsed().as_secs() > 1800 {
            return Err("the background item was not approved".into());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

pub fn unregister_agent() -> Result<(), String> {
    let svc = agent_service();
    let mut err: *mut AnyObject = std::ptr::null_mut();
    let ok: bool = unsafe { msg_send![&*svc, unregisterAndReturnError: &mut err] };
    if ok || status() == SM_NOT_REGISTERED { Ok(()) } else { Err("could not unregister the background item".into()) }
}

// ---------------------------------------------------------------------------
// Launch Services, notifications, clipboard.

pub fn open_gaze() -> Result<(), String> {
    let by_id = Command::new("/usr/bin/open").args(["-b", crate::GAZE_BUNDLE_ID, "--args", "gaze://newtab"]).status().map(|s| s.success()).unwrap_or(false);
    if by_id {
        return Ok(());
    }
    // Just installed: Launch Services may not know the identifier yet.
    let app = crate::paths::Paths::from_env().gaze_bin().ancestors().nth(3).map(|a| a.to_path_buf()).ok_or("F1R3Gaze is not installed")?;
    let st = Command::new("/usr/bin/open").arg("-a").arg(&app).args(["--args", "gaze://newtab"]).status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err("could not open F1R3Gaze".into()) }
}

pub fn reveal(path: &std::path::Path) {
    let _ = Command::new("/usr/bin/open").arg(path).spawn();
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A user notification. `osascript` works from a launch agent without a
/// notification entitlement; H3 may replace it with UNUserNotificationCenter.
pub fn notify(title: &str, body: &str) {
    crate::info!("notification: {title}: {body}");
    let script = format!("display notification {} with title {}", applescript_string(body), applescript_string(title));
    let _ = Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
}

pub fn copy_to_clipboard(s: &str) {
    use std::io::Write;
    if let Ok(mut c) = Command::new("/usr/bin/pbcopy").stdin(std::process::Stdio::piped()).spawn() {
        if let Some(i) = c.stdin.as_mut() {
            let _ = i.write_all(s.as_bytes());
        }
        let _ = c.wait();
    }
}

// ---------------------------------------------------------------------------
// IOPMAssertion: keep the Mac from idle-sleeping during genesis and resizes.

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(kind: *const libc::c_void, level: u32, name: *const libc::c_void, id: *mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

pub fn prevent_idle_sleep(why: &str) -> Option<Awake> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    let kind = CFString::new("PreventUserIdleSystemSleep");
    let name = CFString::new(&format!("ign1t10n: {why}"));
    let mut id = 0u32;
    let r = unsafe { IOPMAssertionCreateWithName(kind.as_concrete_TypeRef() as *const _, 255, name.as_concrete_TypeRef() as *const _, &mut id) };
    (r == 0).then_some(Awake(id))
}

impl Drop for Awake {
    fn drop(&mut self) {
        unsafe {
            IOPMAssertionRelease(self.0);
        }
    }
}
