//! Log files with size-based rotation: `ign1t10n.log` (10 MB x 5) and each
//! node's `<node>.stdout.log` (10 MB x 3), which catches panics and output
//! before the node's own logger starts (spec §5.4).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const MB: u64 = 1024 * 1024;

pub struct Rotating {
    path: PathBuf,
    max: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
}

impl Rotating {
    pub fn new(path: impl Into<PathBuf>, max: u64, keep: usize) -> Rotating {
        let path = path.into();
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Rotating { path, max, keep, file: None, size }
    }

    fn rotated(&self, i: usize) -> PathBuf {
        PathBuf::from(format!("{}.{i}", self.path.display()))
    }

    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(self.rotated(self.keep - 1));
        for i in (1..self.keep - 1).rev() {
            let _ = std::fs::rename(self.rotated(i), self.rotated(i + 1));
        }
        let _ = std::fs::rename(&self.path, self.rotated(1));
        self.size = 0;
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if self.size + bytes.len() as u64 > self.max && self.size > 0 {
            self.rotate();
        }
        if self.file.is_none() {
            self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = &mut self.file {
            if f.write_all(bytes).is_ok() {
                self.size += bytes.len() as u64;
            }
        }
    }
}

static LOG: OnceLock<Mutex<Rotating>> = OnceLock::new();

pub fn init(path: &Path) {
    let _ = LOG.set(Mutex::new(Rotating::new(path, 10 * MB, 5)));
}

pub fn line(level: &str, msg: &str) {
    let l = format!("{} {level:5} [{}] {msg}\n", crate::now_rfc3339(), std::process::id());
    if let Some(m) = LOG.get() {
        if let Ok(mut r) = m.lock() {
            r.write(l.as_bytes());
        }
    }
    if std::env::var_os("IGN1T10N_LOG_STDERR").is_some() || LOG.get().is_none() {
        eprint!("{l}");
    }
}

#[macro_export]
macro_rules! info { ($($t:tt)*) => { $crate::logging::line("INFO", &format!($($t)*)) } }
#[macro_export]
macro_rules! warn { ($($t:tt)*) => { $crate::logging::line("WARN", &format!($($t)*)) } }
#[macro_export]
macro_rules! error { ($($t:tt)*) => { $crate::logging::line("ERROR", &format!($($t)*)) } }

#[cfg(test)]
mod tests {
    #[test]
    fn rotation_keeps_n_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.log");
        let mut r = super::Rotating::new(&p, 10, 3);
        for _ in 0..10 {
            r.write(b"0123456789");
        }
        assert!(p.exists());
        assert!(d.path().join("x.log.1").exists());
        assert!(d.path().join("x.log.2").exists());
        assert!(!d.path().join("x.log.3").exists());
    }
}
