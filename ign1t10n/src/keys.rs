//! Keys (spec §5.5). Every key is secp256k1, generated from the operating
//! system's random source, and stored as an `ENCRYPTED PRIVATE KEY` PEM:
//! PKCS#8 with PBES2, PBKDF2-HMAC-SHA256 (600 000 iterations) and
//! AES-256-CBC, which OpenSSL decrypts and the node's
//! `Secp256k1::parse_pem_file` accepts (Finding "How a key can reach the
//! node"). The password is 32 random bytes, hex, kept in the Keychain.

use crate::paths::write_atomic;
use gaze_wallet::Address;
use k256::ecdsa::SigningKey;
use k256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding, PrivateKeyInfo};
use std::path::Path;

pub const PBKDF2_ITERATIONS: u32 = 600_000;

pub struct Key {
    pub signing: SigningKey,
}

impl Key {
    pub fn generate() -> Key {
        loop {
            let b = crate::random_bytes::<32>();
            if let Ok(k) = SigningKey::from_slice(&b) {
                return Key { signing: k };
            }
        }
    }

    /// The 65-byte uncompressed public key, hex (`04...`, 130 digits).
    pub fn public_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_encoded_point(false).as_bytes())
    }

    pub fn address(&self) -> String {
        Address::from_key(self.signing.verifying_key()).to_string()
    }

    /// The raw 32-byte secret, hex (for Embers' `EMBERS__*_KEY` variables).
    pub fn secret_hex(&self) -> String {
        hex::encode(self.signing.to_bytes())
    }

    pub fn from_secret_hex(s: &str) -> Result<Key, String> {
        let b = hex::decode(s.trim()).map_err(|e| e.to_string())?;
        Ok(Key { signing: SigningKey::from_slice(&b).map_err(|e| e.to_string())? })
    }

    pub fn encrypted_pem(&self, password: &str) -> Result<String, String> {
        let doc = self.signing.to_pkcs8_der().map_err(|e| e.to_string())?;
        let info = PrivateKeyInfo::try_from(doc.as_bytes()).map_err(|e| e.to_string())?;
        let salt = crate::random_bytes::<16>();
        let iv = crate::random_bytes::<16>();
        let params = pkcs5::pbes2::Parameters::pbkdf2_sha256_aes256cbc(PBKDF2_ITERATIONS, &salt, &iv)
            .map_err(|e| e.to_string())?;
        let enc = info.encrypt_with_params(params, password.as_bytes()).map_err(|e| e.to_string())?;
        enc.to_pem("ENCRYPTED PRIVATE KEY", LineEnding::LF).map(|s| s.to_string()).map_err(|e| e.to_string())
    }

    pub fn from_encrypted_pem(pem: &str, password: &str) -> Result<Key, String> {
        let sk = k256::SecretKey::from_pkcs8_encrypted_pem(pem, password.as_bytes()).map_err(|e| format!("cannot decrypt key: {e}"))?;
        Ok(Key { signing: SigningKey::from(sk) })
    }

    pub fn write(&self, path: &Path, password: &str) -> Result<(), String> {
        let pem = self.encrypted_pem(password)?;
        write_atomic(path, pem.as_bytes(), 0o600).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn read(path: &Path, password: &str) -> Result<Key, String> {
        let pem = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Key::from_encrypted_pem(&pem, password)
    }
}

pub fn new_password() -> String {
    hex::encode(crate::random_bytes::<32>())
}

/// The development public keys the node repository ships
/// (`docker/.env.example`: bootstrap/standalone, validators 1 to 4). A
/// generated key must never equal one of these (A5). Public keys only: the
/// release check (`build.sh`) separately ensures no development *private*
/// key is in the package.
pub const DEVELOPMENT_PUBLIC_KEYS: &[&str] = &[
    "04ffc016579a68050d655d55df4e09f04605164543e257c8e6df10361e6068a5336588e9b355ea859c5ab4285a5ef0efdf62bc28b80320ce99e26bb1607b3ad93d",
    "04fa70d7be5eb750e0915c0f6d19e7085d18bb1c22d030feb2a877ca2cd226d04438aa819359c56c720142fbc66e9da03a5ab960a3d8b75363a226b7c800f60420",
    "04837a4cff833e3157e3135d7b40b8e1f33c6e6b5a4342b9fc784230ca4c4f9d356f258debef56ad4984726d6ab3e7709e1632ef079b4bcd653db00b68b2df065f",
    "0457febafcc25dd34ca5e5c025cd445f60e5ea6918931a54eb8c3a204f51760248090b0c757c2bdad7b8c4dca757e109f8ef64737d90712724c8216c94b4ae661c",
    "04d26c6103d7269773b943d7a9c456f9eb227e0d8b1fe30bccee4fca963f4446e3385d99f6386317f2c1ad36b9e6b0d5f97bb0a0041f05781c60a5ebca124a251d",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_round_trips_and_is_pbkdf2_aes() {
        let k = Key::generate();
        let pw = new_password();
        let pem = k.encrypted_pem(&pw).unwrap();
        assert!(pem.starts_with("-----BEGIN ENCRYPTED PRIVATE KEY-----"));
        let back = Key::from_encrypted_pem(&pem, &pw).unwrap();
        assert_eq!(back.public_hex(), k.public_hex());
        assert!(Key::from_encrypted_pem(&pem, "wrong").is_err());
        assert_eq!(k.public_hex().len(), 130);
        assert!(k.address().starts_with("1111"));
        assert!(!DEVELOPMENT_PUBLIC_KEYS.contains(&k.public_hex().as_str()));
    }

    /// Obligation "PEM compatibility": OpenSSL — which the node uses through
    /// `PKey::private_key_from_pem_passphrase` — must decrypt what we write.
    /// Skipped when no `openssl` is on PATH.
    #[test]
    fn openssl_decrypts_our_pem() {
        let Ok(v) = std::process::Command::new("openssl").arg("version").output() else { return };
        if !v.status.success() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let k = Key::generate();
        let pw = new_password();
        let p = dir.path().join("k.pem");
        k.write(&p, &pw).unwrap();
        let out = std::process::Command::new("openssl")
            .args(["pkey", "-in", p.to_str().unwrap(), "-passin", &format!("pass:{pw}"), "-text", "-noout"])
            .output()
            .unwrap();
        assert!(out.status.success(), "openssl: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout).replace([':', ' ', '\n'], "");
        assert!(text.contains(&k.public_hex()), "openssl sees the same public key");
        assert!(String::from_utf8_lossy(&out.stdout).contains("secp256k1"));
    }
}
