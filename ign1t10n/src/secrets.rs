//! Secrets: one password per PEM and the Embers secrets, under Keychain
//! service `io.f1r3fly.ign1t10n` (spec §5.5). The item's access control list
//! is the application's designated requirement; the menu bar, supervisor
//! and `ctl` are one binary and read it without a prompt.
//!
//! Off macOS (CI, development) a 0600 JSON file in the state directory
//! stands in; it is never used on a Mac unless `IGN1T10N_DEV_SECRETS=1`.

use crate::paths::{Paths, write_atomic};
use std::collections::BTreeMap;

pub const SERVICE: &str = "io.f1r3fly.ign1t10n";

pub trait Secrets: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
    fn accounts(&self) -> Result<Vec<String>, String>;
}

/// The password account for a PEM: `pem.bootstrap`, `pem.validator-3`, ...
pub fn pem_account(name: &str) -> String {
    format!("pem.{name}")
}

pub fn open(paths: &Paths) -> Box<dyn Secrets> {
    let dev = std::env::var("IGN1T10N_DEV_SECRETS").map(|v| v == "1").unwrap_or(false);
    #[cfg(target_os = "macos")]
    if !dev {
        return Box::new(crate::platform::macos::Keychain);
    }
    let _ = dev;
    Box::new(FileSecrets { path: paths.secrets_file() })
}

/// Fetch a password, creating it if absent.
pub fn password(s: &dyn Secrets, name: &str) -> Result<String, String> {
    let acct = pem_account(name);
    if let Some(p) = s.get(&acct)? {
        return Ok(p);
    }
    let p = crate::keys::new_password();
    s.set(&acct, &p)?;
    Ok(p)
}

pub struct FileSecrets {
    pub path: std::path::PathBuf,
}

impl FileSecrets {
    fn load(&self) -> Result<BTreeMap<String, String>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn store(&self, m: &BTreeMap<String, String>) -> Result<(), String> {
        write_atomic(&self.path, serde_json::to_string_pretty(m).unwrap().as_bytes(), 0o600).map_err(|e| e.to_string())
    }
}

impl Secrets for FileSecrets {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.load()?.get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        let mut m = self.load()?;
        m.insert(account.into(), value.into());
        self.store(&m)
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        let mut m = self.load()?;
        m.remove(account);
        self.store(&m)
    }
    fn accounts(&self) -> Result<Vec<String>, String> {
        Ok(self.load()?.keys().cloned().collect())
    }
}
