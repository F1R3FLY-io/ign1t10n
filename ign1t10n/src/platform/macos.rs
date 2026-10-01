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
// The supervisor's launch agent.
//
// A classic per-user launch agent: ~/Library/LaunchAgents/<label>.plist with
// the supervisor's ABSOLUTE path, loaded with `launchctl bootstrap`. The
// spec chose SMAppService (§5.2), which stores the program relative to the
// app and finds the app through the background-task database by entry id;
// after a reinstall launchd kept a stale id, could not resolve the path
// ("copy_bundle_path … Invalid or missing Program/ProgramArguments") and
// never started the supervisor again. An absolute path has no such link to
// break. macOS still lists the agent under Login Items, where the person can
// switch it off; ign1t10n then says so.

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

const SM_NOT_REGISTERED: isize = 0;

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn domain() -> String {
    format!("gui/{}", uid())
}

fn target() -> String {
    format!("{}/{}", domain(), crate::AGENT_LABEL)
}

pub fn agent_plist_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    home.join("Library/LaunchAgents").join(format!("{}.plist", crate::AGENT_LABEL))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The agent's plist for the supervisor at `exe`.
pub fn agent_plist(exe: &std::path::Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by ign1t10n; rewritten when ign1t10n moves or is updated. -->
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>supervise</string>
  </array>
  <key>AssociatedBundleIdentifiers</key><array><string>{bundle}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Background</string>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>/dev/null</string>
  <key>StandardErrorPath</key><string>/dev/null</string>
</dict>
</plist>
"#,
        label = crate::AGENT_LABEL,
        exe = xml_escape(&exe.display().to_string()),
        bundle = crate::BUNDLE_ID,
    )
}

fn launchctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("/bin/launchctl").args(args).output().map_err(|e| e.to_string())?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() { Ok(text) } else { Err(format!("launchctl {}: {}", args.join(" "), text.trim())) }
}

fn loaded() -> bool {
    launchctl(&["print", &target()]).is_ok()
}

/// What the loaded job will run, if it is loaded.
fn loaded_program() -> Option<String> {
    let text = launchctl(&["print", &target()]).ok()?;
    let mut lines = text.lines();
    lines.find(|l| l.trim_start().starts_with("arguments = {"))?;
    lines.next().map(|l| l.trim().to_string())
}

/// Remove a registration made by earlier versions through SMAppService
/// (it holds the same label and, after a reinstall, cannot start).
fn retire_smappservice() {
    let name = NSString::from_str(crate::AGENT_PLIST);
    let svc: Retained<AnyObject> = unsafe { msg_send_id![class!(SMAppService), agentServiceWithPlistName: &*name] };
    let status: isize = unsafe { msg_send![&*svc, status] };
    if status != SM_NOT_REGISTERED {
        let mut err: *mut AnyObject = std::ptr::null_mut();
        let _: bool = unsafe { msg_send![&*svc, unregisterAndReturnError: &mut err] };
        crate::info!("removed the earlier SMAppService registration of the supervisor");
    }
}

pub fn open_login_items() {
    let _ = Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.LoginItems-Settings.extension").status();
}

/// S6, and whenever the supervisor must run: make sure the agent's plist
/// names this copy of ign1t10n, and that launchd has it loaded.
pub fn register_agent(waiting: &dyn Fn(&str)) -> Result<(), String> {
    retire_smappservice();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let path = agent_plist_path();
    let want = agent_plist(&exe);
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    let exe_s = exe.display().to_string();
    if have == want && loaded() && loaded_program().as_deref() == Some(exe_s.as_str()) {
        return Ok(());
    }
    crate::paths::write_atomic(&path, want.as_bytes(), 0o644).map_err(|e| format!("{}: {e}", path.display()))?;
    if loaded() {
        let _ = launchctl(&["bootout", &target()]);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    match launchctl(&["bootstrap", &domain(), &path.display().to_string()]) {
        Ok(_) => {
            crate::info!("launch agent loaded: {exe_s} supervise");
            Ok(())
        }
        Err(e) => {
            // Usually: switched off under Login Items ("Operation not permitted").
            waiting("ign1t10n's background item is switched off. Turn on ign1t10n in System Settings › General › Login Items › Allow in the Background.");
            open_login_items();
            Err(format!("{e}. If ign1t10n is switched off in System Settings › General › Login Items, switch it on and try again."))
        }
    }
}

pub fn agent_enabled() -> Option<bool> {
    Some(loaded())
}

/// Start the supervisor: ensure the agent is current and loaded (loading it
/// starts it, RunAtLoad), then kickstart in case it was loaded but stopped.
pub fn start_agent() -> Result<(), String> {
    register_agent(&|w| crate::info!("{w}"))?;
    let _ = launchctl(&["kickstart", &target()]);
    Ok(())
}

pub fn unregister_agent() -> Result<(), String> {
    retire_smappservice();
    if loaded() {
        launchctl(&["bootout", &target()])?;
    }
    let _ = std::fs::remove_file(agent_plist_path());
    Ok(())
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
