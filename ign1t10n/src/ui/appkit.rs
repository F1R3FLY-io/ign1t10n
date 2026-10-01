//! The menu-bar application on AppKit (spec §10): a status item whose menu
//! is rebuilt from `ui::menu` every two seconds, the first-run window
//! (installation options, then provisioning progress), the configuration
//! panel, and the Fund / Reset / Uninstall dialogues.
//!
//! NOT COMPILED IN THE AUTHORING ENVIRONMENT (Linux, no Apple target).
//! Written against objc2 0.5.2 / objc2-app-kit 0.2.2; the `macos` CI job
//! builds it. Everything it shows comes from the tested model in `ui`.

use super::{Action, Dot, Entry, Load};
use crate::control::{self, Report, Request};
use crate::paths::Paths;
use crate::provision::{self, Choices, Progress, Stage, StageStatus};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{declare_class, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::*;
use objc2_foundation::{MainThreadMarker, NSObject, NSPoint, NSRect, NSSize, NSString, NSTimer};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Bring the app forward. `-[NSApplication activate]` is macOS 14+, and the
/// minimum is 13 (Decision 3), so the older call stays.
#[allow(deprecated, unused_unsafe)]
fn activate(mtm: MainThreadMarker) {
    unsafe { NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true) };
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Provisioning progress, written by the provisioning thread, read by the timer.
#[derive(Default)]
struct Shared {
    stages: Vec<(Stage, StageStatus)>,
    finished: Option<Result<(), String>>,
}

struct UiProgress(Arc<Mutex<Shared>>);

impl Progress for UiProgress {
    fn update(&self, stage: Stage, status: StageStatus) {
        provision::Quiet.update(stage, status.clone());
        let mut g = self.0.lock().unwrap();
        match g.stages.iter_mut().find(|(s, _)| *s == stage) {
            Some(x) => x.1 = status,
            None => g.stages.push((stage, status)),
        }
    }
}

/// The widgets of the options view shared by first run and Configure.
struct Options {
    view: Retained<NSView>,
    stepper: Retained<NSStepper>,
    count: Retained<NSTextField>,
    estimate: Retained<NSTextField>,
    tolerance: Retained<NSTextField>,
    embers: Retained<NSButton>,
    gaze: Retained<NSButton>,
}

pub struct Ivars {
    paths: Paths,
    item: RefCell<Option<Retained<NSStatusItem>>>,
    actions: RefCell<Vec<Action>>,
    report: RefCell<Option<Report>>,
    shared: Arc<Mutex<Shared>>,
    window: RefCell<Option<Retained<NSWindow>>>,
    rows: RefCell<Vec<Retained<NSTextField>>>,
    detail: RefCell<Option<Retained<NSTextField>>>,
    options: RefCell<Option<Options>>,
    provisioning: RefCell<bool>,
}

declare_class!(
    pub struct Delegate;

    unsafe impl ClassType for Delegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "Ign1t10nDelegate";
    }

    impl DeclaredClass for Delegate {
        type Ivars = Ivars;
    }

    unsafe impl Delegate {
        #[method(tick:)]
        fn tick(&self, _t: Option<&AnyObject>) {
            self.refresh();
        }

        #[method(menuAction:)]
        fn menu_action(&self, sender: &NSMenuItem) {
            let i = unsafe { sender.tag() } as usize;
            let a = self.ivars().actions.borrow().get(i).cloned();
            if let Some(a) = a {
                self.perform(a);
            }
        }

        #[method(stepperChanged:)]
        fn stepper_changed(&self, _s: Option<&AnyObject>) {
            self.update_estimate();
        }

        #[method(toggleChanged:)]
        fn toggle_changed(&self, _s: Option<&AnyObject>) {
            self.update_estimate();
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, paths: Paths) -> Retained<Self> {
        let this = mtm.alloc::<Delegate>().set_ivars(Ivars {
            paths,
            item: RefCell::new(None),
            actions: RefCell::new(vec![]),
            report: RefCell::new(None),
            shared: Arc::default(),
            window: RefCell::new(None),
            rows: RefCell::new(vec![]),
            detail: RefCell::new(None),
            options: RefCell::new(None),
            provisioning: RefCell::new(false),
        });
        unsafe { msg_send_id![super(this), init] }
    }

    fn mtm(&self) -> MainThreadMarker {
        MainThreadMarker::from(self)
    }

    fn target(&self) -> &AnyObject {
        self
    }

    // ------------------------------------------------------------------
    // Status item and menu

    fn install_status_item(&self) {
        let mtm = self.mtm();
        unsafe {
            let bar = NSStatusBar::systemStatusBar();
            let item = bar.statusItemWithLength(NSVariableStatusItemLength);
            if let Some(b) = item.button(mtm) {
                b.setTitle(&ns("🔥"));
            }
            let menu = NSMenu::new(mtm);
            item.setMenu(Some(&menu));
            *self.ivars().item.borrow_mut() = Some(item);
        }
    }

    fn refresh(&self) {
        let p = &self.ivars().paths;
        let report = control::call(p, &Request::Status, Duration::from_secs(2)).ok().and_then(|r| r.status);
        self.rebuild_menu(report.as_ref());
        *self.ivars().report.borrow_mut() = report;
        self.refresh_first_run();
    }

    fn rebuild_menu(&self, r: Option<&Report>) {
        let mtm = self.mtm();
        let Some(item) = self.ivars().item.borrow().clone() else { return };
        let title = match r.map(|r| super::dot(&r.shard)) {
            None | Some(Dot::Hollow) => "🔥○",
            Some(Dot::None) => "🔥",
            Some(Dot::Amber) => "🔥🟠",
            Some(Dot::Red) => "🔥🔴",
        };
        unsafe {
            if let Some(b) = item.button(mtm) {
                b.setTitle(&ns(title));
            }
            let Some(menu) = item.menu(mtm) else { return };
            menu.removeAllItems();
            let mut actions = vec![];
            for e in super::menu(r) {
                match e {
                    Entry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                    Entry::Info(t) => {
                        let mi = NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), &ns(&t), None, &ns(""));
                        mi.setEnabled(false);
                        menu.addItem(&mi);
                    }
                    Entry::Item(t, a, enabled) => {
                        let mi = NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), &ns(&t), Some(sel!(menuAction:)), &ns(""));
                        mi.setTarget(Some(self.target()));
                        mi.setTag(actions.len() as isize);
                        mi.setEnabled(enabled);
                        menu.addItem(&mi);
                        actions.push(a);
                    }
                }
            }
            *self.ivars().actions.borrow_mut() = actions;
        }
    }

    // ------------------------------------------------------------------
    // Actions

    fn call(&self, r: Request) {
        match control::call(&self.ivars().paths, &r, Duration::from_secs(30)) {
            Ok(x) if x.ok => {}
            Ok(x) => self.alert("ign1t10n", &x.error.unwrap_or_default(), &["OK"]).ignore(),
            Err(e) => self.alert("The shard supervisor is not running", &e, &["OK"]).ignore(),
        }
        self.refresh();
    }

    fn perform(&self, a: Action) {
        let p = self.ivars().paths.clone();
        match a {
            Action::OpenGaze => {
                if let Err(e) = crate::platform::open_gaze() {
                    self.alert("Could not open F1R3Gaze", &e, &["OK"]).ignore();
                }
            }
            Action::Start => {
                if let Err(e) = provision::ensure_supervisor(&p, false) {
                    self.alert("The shard supervisor could not be started", &e, &["OK"]).ignore();
                    return;
                }
                self.call(Request::Start)
            }
            Action::Stop => self.call(Request::Stop),
            Action::Retry => self.call(Request::Retry),
            Action::Undo => self.call(Request::Undo),
            Action::Configure => self.configure(),
            Action::Fund => self.fund(),
            Action::CopyEndpoints => {
                if let Some(r) = self.ivars().report.borrow().as_ref() {
                    crate::platform::copy_to_clipboard(&super::endpoints(r));
                }
            }
            Action::ShowLogs => crate::platform::reveal(&p.logs),
            Action::Reset => {
                let n = self.ivars().report.borrow().as_ref().map(|r| r.validators).unwrap_or(2);
                if self.alert("Reset the local shard?", "Every deploy, registry entry and balance on the local shard is erased and a new genesis is made. Your F1R3Gaze wallets are kept and funded again. The old data is archived.", &["Reset", "Cancel"]) == 0 {
                    self.call(Request::Resize { target: n, new_shard: true });
                }
            }
            Action::Uninstall => {
                if self.alert("Uninstall ign1t10n?", "The local shard, its logs and its keys are removed. F1R3Gaze and its wallets are left alone.", &["Uninstall", "Cancel"]) == 0 {
                    let _ = control::call(&p, &Request::Shutdown, Duration::from_secs(10));
                    for _ in 0..120 {
                        if !p.socket().exists() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    let _ = crate::platform::unregister_agent();
                    let s = crate::secrets::open(&p);
                    let _ = crate::lifecycle::uninstall_files(&p, &*s, false);
                    if let Some(app) = std::env::current_exe().ok().and_then(|e| e.ancestors().nth(3).map(|a| a.to_path_buf())) {
                        let script = format!("tell application \"Finder\" to delete POSIX file \"{}\"", app.display());
                        let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).status();
                    }
                    unsafe { NSApplication::sharedApplication(self.mtm()).terminate(None) };
                }
            }
            Action::Quit => {
                // The shard keeps running; quitting only removes the menu.
                unsafe { NSApplication::sharedApplication(self.mtm()).terminate(None) };
            }
        }
    }

    fn fund(&self) {
        let mtm = self.mtm();
        let wallet = self.ivars().report.borrow().as_ref().and_then(|r| r.funded_wallet.clone()).unwrap_or_default();
        unsafe {
            let v = NSView::initWithFrame(mtm.alloc(), rect(0.0, 0.0, 380.0, 58.0));
            let addr = NSTextField::initWithFrame(mtm.alloc(), rect(0.0, 32.0, 380.0, 24.0));
            addr.setStringValue(&ns(&wallet));
            let amount = NSTextField::initWithFrame(mtm.alloc(), rect(0.0, 0.0, 120.0, 24.0));
            amount.setStringValue(&ns("1000"));
            let unit = NSTextField::labelWithString(&ns("F1R3"), mtm);
            unit.setFrame(rect(128.0, 3.0, 60.0, 20.0));
            v.addSubview(&addr);
            v.addSubview(&amount);
            v.addSubview(&unit);
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&ns("Fund a wallet from the local faucet"));
            alert.setInformativeText(&ns("Paste a F1R3Gaze wallet address."));
            alert.setAccessoryView(Some(&v));
            alert.addButtonWithTitle(&ns("Send"));
            alert.addButtonWithTitle(&ns("Cancel"));
            if alert.runModal() != NSAlertFirstButtonReturn {
                return;
            }
            let address = addr.stringValue().to_string().trim().to_string();
            let f1r3: i64 = amount.stringValue().to_string().trim().parse().unwrap_or(0);
            let p = self.ivars().paths.clone();
            // Waits for finalisation; off the main thread.
            std::thread::spawn(move || {
                let r = control::call(&p, &Request::Fund { address, f1r3 }, Duration::from_secs(600));
                let msg = match r {
                    Ok(x) if x.ok => x.message.unwrap_or_default(),
                    Ok(x) => format!("failed: {}", x.error.unwrap_or_default()),
                    Err(e) => format!("failed: {e}"),
                };
                crate::platform::notify("Fund a wallet", &msg);
            });
        }
    }

    // ------------------------------------------------------------------
    // The options view: validators, Embers, F1R3Gaze

    fn options_view(&self, n: u8, embers: bool, gaze: bool) -> Options {
        let mtm = self.mtm();
        unsafe {
            let view = NSView::initWithFrame(mtm.alloc(), rect(0.0, 0.0, 420.0, 150.0));
            let label = NSTextField::labelWithString(&ns("Validators"), mtm);
            label.setFrame(rect(0.0, 124.0, 90.0, 20.0));
            let count = NSTextField::labelWithString(&ns(&n.to_string()), mtm);
            count.setFrame(rect(96.0, 124.0, 30.0, 20.0));
            let stepper = NSStepper::initWithFrame(mtm.alloc(), rect(126.0, 120.0, 20.0, 28.0));
            stepper.setMinValue(crate::MIN_VALIDATORS as f64);
            stepper.setMaxValue(crate::MAX_VALIDATORS as f64);
            stepper.setIncrement(1.0);
            stepper.setIntegerValue(n as isize);
            stepper.setTarget(Some(self.target()));
            stepper.setAction(Some(sel!(stepperChanged:)));
            let estimate = NSTextField::wrappingLabelWithString(&ns(""), mtm);
            estimate.setFrame(rect(0.0, 78.0, 420.0, 40.0));
            let tolerance = NSTextField::labelWithString(&ns(""), mtm);
            tolerance.setFrame(rect(0.0, 58.0, 420.0, 18.0));
            let e = NSButton::checkboxWithTitle_target_action(&ns("Bundle Embers (wallet balances, history and transfers in F1R3Gaze)"), Some(self.target()), Some(sel!(toggleChanged:)), mtm);
            e.setFrame(rect(0.0, 30.0, 420.0, 20.0));
            e.setState(if embers { NSControlStateValueOn } else { NSControlStateValueOff });
            let g = NSButton::checkboxWithTitle_target_action(&ns("Use the local shard in F1R3Gaze"), Some(self.target()), Some(sel!(toggleChanged:)), mtm);
            g.setFrame(rect(0.0, 4.0, 420.0, 20.0));
            g.setState(if gaze { NSControlStateValueOn } else { NSControlStateValueOff });
            for sub in [&*label as &NSView, &count, &stepper, &estimate, &tolerance, &e, &g] {
                view.addSubview(sub);
            }
            Options { view, stepper, count, estimate, tolerance, embers: e, gaze: g }
        }
    }

    fn read_options(o: &Options) -> Choices {
        unsafe {
            Choices {
                validators: o.stepper.integerValue().clamp(crate::MIN_VALIDATORS as isize, crate::MAX_VALIDATORS as isize) as u8,
                embers: o.embers.state() == NSControlStateValueOn,
                gaze_integration: o.gaze.state() == NSControlStateValueOn,
            }
        }
    }

    fn update_estimate(&self) {
        let b = self.ivars().options.borrow();
        let Some(o) = b.as_ref() else { return };
        let c = Self::read_options(o);
        let e = super::estimate(c.validators, c.embers, crate::platform::physical_memory(), crate::platform::free_disk(&self.ivars().paths.state));
        unsafe {
            o.count.setStringValue(&ns(&c.validators.to_string()));
            let warn = match e.load {
                Load::Fine => "",
                Load::Warn => "  ⚠ more than half this Mac's memory.",
                Load::Confirm => "  ⚠ more than 80% of this Mac's memory.",
            };
            o.estimate.setStringValue(&ns(&format!("{}{warn}", e.text)));
            o.tolerance.setStringValue(&ns(&super::tolerance_text(c.validators)));
        }
    }

    /// Show the options in an alert; `None` if cancelled.
    fn ask_options(&self, title: &str, info: &str, ok: &str, c: &Choices) -> Option<Choices> {
        let o = self.options_view(c.validators, c.embers, c.gaze_integration);
        let view = o.view.clone();
        *self.ivars().options.borrow_mut() = Some(o);
        self.update_estimate();
        let mtm = self.mtm();
        let res = unsafe {
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&ns(title));
            alert.setInformativeText(&ns(info));
            alert.setAccessoryView(Some(&view));
            alert.addButtonWithTitle(&ns(ok));
            alert.addButtonWithTitle(&ns("Cancel"));
            activate(mtm);
            alert.runModal()
        };
        let chosen = self.ivars().options.borrow_mut().take().map(|o| Self::read_options(&o));
        if res != NSAlertFirstButtonReturn {
            return None;
        }
        let c = chosen?;
        let e = super::estimate(c.validators, c.embers, crate::platform::physical_memory(), None);
        if e.load == Load::Confirm && self.alert("This uses most of this Mac's memory", &e.text, &["Continue", "Cancel"]) != 0 {
            return None;
        }
        Some(c)
    }

    fn configure(&self) {
        let Some(r) = self.ivars().report.borrow().clone() else { return };
        let cur = Choices { validators: r.validators, embers: r.embers_enabled, gaze_integration: r.gaze_integration };
        let Some(c) = self.ask_options("Configure the local shard", "Changes apply to the running shard.", "Apply", &cur) else { return };
        if c.embers != cur.embers || c.gaze_integration != cur.gaze_integration {
            self.call(Request::SetOptions {
                embers: (c.embers != cur.embers).then_some(c.embers),
                gaze_integration: (c.gaze_integration != cur.gaze_integration).then_some(c.gaze_integration),
            });
        }
        if c.validators != cur.validators {
            let how = self.alert(
                &format!("Change to {} validators", c.validators),
                "In place keeps every deploy and balance; validators join or leave one at a time (a few minutes each). A new shard starts over with a new genesis and archives the old data.",
                &["In place", "New shard", "Cancel"],
            );
            match how {
                0 => self.call(Request::Resize { target: c.validators, new_shard: false }),
                1 => self.call(Request::Resize { target: c.validators, new_shard: true }),
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------
    // First run

    fn first_run(&self) {
        let p = self.ivars().paths.clone();
        let existing = crate::manifest::Manifest::load(&p).ok().flatten();
        let choices = match &existing {
            // Resuming an interrupted provisioning: keep the recorded choices.
            Some(m) => Choices { validators: m.shard.genesis_validators, embers: m.options.embers, gaze_integration: m.options.gaze_integration },
            None => {
                let elsewhere = crate::gaze::points_elsewhere(&p.profile);
                let d = Choices { gaze_integration: !elsewhere, ..Choices::default() };
                let info = if elsewhere {
                    "ign1t10n will set up a local shard on this Mac. F1R3Gaze currently points at another shard; tick the last box to switch it to the local one."
                } else {
                    "ign1t10n will set up a local shard on this Mac: a bootstrap node, validators, an observer and, optionally, Embers, all on 127.0.0.1."
                };
                match self.ask_options("Set up your local shard", info, "Install", &d) {
                    Some(c) => c,
                    None => {
                        unsafe { NSApplication::sharedApplication(self.mtm()).terminate(None) };
                        return;
                    }
                }
            }
        };
        self.show_progress_window();
        *self.ivars().provisioning.borrow_mut() = true;
        let shared = self.ivars().shared.clone();
        std::thread::spawn(move || {
            let secrets = crate::secrets::open(&p);
            let progress = UiProgress(shared.clone());
            let run = provision::Run { paths: &p, secrets: &*secrets, choices, progress: &progress, headless: false, open: true };
            let r = provision::provision(&run).map(|_| ());
            shared.lock().unwrap().finished = Some(r);
        });
    }

    fn show_progress_window(&self) {
        let mtm = self.mtm();
        unsafe {
            let w = NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                rect(0.0, 0.0, 520.0, 330.0),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::NSBackingStoreBuffered,
                false,
            );
            w.setReleasedWhenClosed(false);
            w.setTitle(&ns("Setting up your local shard"));
            let content = w.contentView().unwrap();
            let mut rows = vec![];
            for (i, s) in Stage::ALL.iter().enumerate() {
                let l = NSTextField::labelWithString(&ns(&format!("○  {}", s.title())), mtm);
                l.setFrame(rect(24.0, 290.0 - 24.0 * i as f64, 470.0, 20.0));
                content.addSubview(&l);
                rows.push(l);
            }
            let detail = NSTextField::wrappingLabelWithString(&ns(""), mtm);
            detail.setFrame(rect(24.0, 10.0, 470.0, 44.0));
            content.addSubview(&detail);
            w.center();
            w.makeKeyAndOrderFront(None);
            activate(mtm);
            *self.ivars().rows.borrow_mut() = rows;
            *self.ivars().detail.borrow_mut() = Some(detail);
            *self.ivars().window.borrow_mut() = Some(w);
        }
    }

    fn refresh_first_run(&self) {
        if !*self.ivars().provisioning.borrow() {
            return;
        }
        let (stages, finished) = {
            let g = self.ivars().shared.lock().unwrap();
            (g.stages.clone(), g.finished.clone())
        };
        let rows = self.ivars().rows.borrow();
        let mut detail = String::new();
        unsafe {
            for (i, s) in Stage::ALL.iter().enumerate() {
                let st = stages.iter().find(|(x, _)| x == s).map(|x| x.1.clone()).unwrap_or(StageStatus::Pending);
                let (mark, extra) = match &st {
                    StageStatus::Pending => ("○", String::new()),
                    StageStatus::Running(d) => ("◐", d.clone()),
                    StageStatus::Done => ("●", String::new()),
                    StageStatus::Failed(e) => ("✗", e.clone()),
                    StageStatus::Waiting(w) => ("⏸", w.clone()),
                };
                if !extra.is_empty() {
                    detail = extra;
                }
                if let Some(l) = rows.get(i) {
                    l.setStringValue(&ns(&format!("{mark}  {}", s.title())));
                }
            }
            if let Some(d) = self.ivars().detail.borrow().as_ref() {
                d.setStringValue(&ns(&detail));
            }
        }
        drop(rows);
        if let Some(r) = finished {
            *self.ivars().provisioning.borrow_mut() = false;
            match r {
                Ok(()) => {
                    if let Some(w) = self.ivars().window.borrow_mut().take() {
                        w.close();
                    }
                }
                Err(e) => {
                    // Keep the window; provisioning resumes at the failed stage.
                    if self.alert("Setup stopped", &format!("{e}\n\nFix the cause and choose Try again; setup resumes where it stopped."), &["Try again", "Close"]) == 0 {
                        self.ivars().shared.lock().unwrap().finished = None;
                        if let Some(w) = self.ivars().window.borrow_mut().take() {
                            w.close();
                        }
                        self.first_run();
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------

    /// A modal alert; returns the index of the button chosen.
    fn alert(&self, title: &str, text: &str, buttons: &[&str]) -> usize {
        let mtm = self.mtm();
        unsafe {
            let a = NSAlert::new(mtm);
            a.setMessageText(&ns(title));
            a.setInformativeText(&ns(text));
            for b in buttons {
                a.addButtonWithTitle(&ns(b));
            }
            activate(mtm);
            (a.runModal() - NSAlertFirstButtonReturn).max(0) as usize
        }
    }
}

trait Ignore {
    fn ignore(self);
}
impl Ignore for usize {
    fn ignore(self) {}
}

pub fn run(first_run: bool) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("the menu bar must run on the main thread")?;
    let paths = Paths::from_env();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = Delegate::new(mtm, paths.clone());
    delegate.install_status_item();
    let provisioned = crate::manifest::Manifest::load(&paths).ok().flatten().map(|m| m.stages.complete()).unwrap_or(false);
    // --first-run comes from the installer: resume setup if unfinished, and
    // in any case end with F1R3Gaze open on the running shard.
    if !provisioned {
        delegate.first_run();
    } else {
        // Setup is done: on every launch (including after a reinstall) make
        // sure the supervisor runs on this copy of ign1t10n, without waiting
        // for a menu click. Off the main thread: it may wait for launchd.
        let p = paths.clone();
        std::thread::spawn(move || {
            // Keep the launch agent pointing at this copy (idempotent; also
            // retires an SMAppService registration from earlier versions).
            if let Err(e) = crate::platform::register_agent(&|w| crate::info!("{w}")) {
                crate::warn!("launch agent: {e}");
            }
            if let Err(e) = provision::ensure_supervisor(&p, false) {
                crate::warn!("could not start the shard supervisor: {e}");
                crate::platform::notify("Local shard", &format!("The shard supervisor could not be started: {e}"));
                return;
            }
            if first_run {
                // Installed or updated: open the browser once the shard runs.
                let running = crate::admin::wait_for("the shard to run", Duration::from_secs(600), Duration::from_secs(2), || {
                    control::call(&p, &Request::Status, Duration::from_secs(5))
                        .ok()
                        .and_then(|r| r.status)
                        .filter(|st| matches!(st.shard, crate::control::ShardState::Running | crate::control::ShardState::Degraded(_)))
                        .map(|_| ())
                });
                match running {
                    Ok(()) => {
                        if let Err(e) = crate::platform::open_gaze() {
                            crate::warn!("could not open F1R3Gaze: {e}");
                        }
                    }
                    Err(e) => crate::warn!("not opening F1R3Gaze: {e}"),
                }
            }
        });
    }
    delegate.refresh();
    let target: &AnyObject = &delegate;
    let tick: Sel = sel!(tick:);
    unsafe {
        let _timer = NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(2.0, target, tick, None, true);
        app.run();
    }
    drop(delegate);
    Ok(())
}
