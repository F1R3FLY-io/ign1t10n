//! A stand-in for `f1r3drive`: reads the positional mount directory,
//! drops a `.f1r3drive-fake-mount` marker (`drive::up` watches for
//! this when `IGN1T10N_FAKE_MOUNTS=1` so the test doesn't need real
//! FUSE), and sleeps until SIGTERM. Writes under the mount dir are
//! served by the OS as a plain directory, which lets the end-to-end
//! test write + read back a file without a FUSE kernel driver.

use std::sync::atomic::{AtomicBool, Ordering};

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

fn main() {
    unsafe {
        libc::signal(libc::SIGTERM, on_term as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as libc::sighandler_t);
    }
    let args: Vec<String> = std::env::args().collect();
    let mount = args.get(1).cloned().expect("mount directory is positional arg");
    std::fs::create_dir_all(&mount).unwrap();
    // The marker file `drive::up` polls for when IGN1T10N_FAKE_MOUNTS=1.
    // Real FUSE would replace the directory contents with a mounted
    // view; here we just drop the marker so the test can proceed.
    std::fs::write(format!("{mount}/.f1r3drive-fake-mount"), b"").unwrap();
    eprintln!("fake f1r3drive mounted at {mount}");
    while !TERM.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // Clean up the marker so a `down` + `up` cycle re-arms the probe.
    let _ = std::fs::remove_file(format!("{mount}/.f1r3drive-fake-mount"));
}
