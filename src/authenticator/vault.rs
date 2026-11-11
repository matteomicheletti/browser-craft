//! The authenticator vault file: the accounts and settings as JSON, sealed
//! with XChaCha20-Poly1305 under a key derived from the master password with
//! Argon2id. The same container (another magic) is the encrypted backup.
//!
//! Layout (all integers little-endian), 63-byte header then ciphertext:
//!
//! ```text
//!  0  8  magic     "BCOTPVLT" (vault) or "BCOTPBAK" (backup)
//!  8  2  version   1
//! 10  1  kdf       1 = Argon2id v1.3
//! 11  4  memory    KiB
//! 15  4  passes
//! 19  4  lanes
//! 23 16  salt      random per password
//! 39 24  nonce     random per write
//! 63  …  ciphertext + 16-byte Poly1305 tag
//! ```
//!
//! The whole header is the AEAD's associated data, so changing any byte of
//! the file (parameters included) makes decryption fail.

use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::Account;

pub const VAULT_MAGIC: &[u8; 8] = b"BCOTPVLT";
pub const BACKUP_MAGIC: &[u8; 8] = b"BCOTPBAK";
/// The format this code writes; older ones are read, newer ones refused.
pub const VERSION: u16 = 1;
const KDF_ARGON2ID: u8 = 1;
const HEADER: usize = 63;
const SALT: usize = 16;
const NONCE: usize = 24;
const TAG: usize = 16;
/// Largest vault or backup read (thousands of accounts fit in far less).
pub const MAX_FILE: u64 = 16 << 20;
/// Shortest master password accepted for a new vault or backup.
pub const MIN_PASSWORD: usize = 8;

/// Argon2id cost. New files use [`KdfParams::STRONG`] (RFC 9106's second
/// recommended option: 64 MiB, 3 passes, 4 lanes); files are read with the
/// parameters they carry, within [`KdfParams::check`]'s bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
}

impl KdfParams {
    pub const STRONG: KdfParams = KdfParams {
        memory_kib: 64 * 1024,
        passes: 3,
        lanes: 4,
    };
    /// Cheap parameters so the test suite stays fast.
    #[cfg(test)]
    pub const TEST: KdfParams = KdfParams {
        memory_kib: 64,
        passes: 1,
        lanes: 1,
    };

    /// The parameters new files get.
    pub fn for_new() -> KdfParams {
        #[cfg(test)]
        return KdfParams::TEST;
        #[cfg(not(test))]
        KdfParams::STRONG
    }

    /// Refuses parameters a crafted file could use to exhaust memory or
    /// time (at most 1 GiB, 64 passes, 16 lanes).
    fn check(&self) -> Result<(), String> {
        let ok = (1..=16).contains(&self.lanes)
            && (8 * self.lanes..=1024 * 1024).contains(&self.memory_kib)
            && (1..=64).contains(&self.passes);
        if ok {
            Ok(())
        } else {
            Err("The file asks for key derivation settings outside the allowed range.".into())
        }
    }
}

/// A derived encryption key with the salt and parameters it came from.
/// Wiped when dropped; never printed.
#[derive(Clone)]
pub struct VaultKey {
    key: Zeroizing<[u8; 32]>,
    salt: [u8; SALT],
    params: KdfParams,
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultKey")
            .field("key", &"<redacted>")
            .field("params", &self.params)
            .finish()
    }
}

impl VaultKey {
    /// Derives a key for a new password with a fresh random salt. Slow by
    /// design (about 50 ms for the strong parameters in a release build on
    /// a desktop CPU, seconds in a debug build): run it off the UI thread.
    pub fn new(password: &str) -> Result<VaultKey, String> {
        if password.chars().count() < MIN_PASSWORD {
            return Err(format!("Use a password of at least {MIN_PASSWORD} characters."));
        }
        let mut salt = [0u8; SALT];
        random(&mut salt)?;
        VaultKey::derive(password, salt, KdfParams::for_new())
    }

    fn derive(password: &str, salt: [u8; SALT], params: KdfParams) -> Result<VaultKey, String> {
        params.check()?;
        let config = argon2::Config {
            variant: argon2::Variant::Argon2id,
            version: argon2::Version::Version13,
            mem_cost: params.memory_kib,
            time_cost: params.passes,
            lanes: params.lanes,
            thread_mode: argon2::ThreadMode::from_threads(params.lanes),
            secret: &[],
            ad: &[],
            hash_length: 32,
        };
        let mut raw = argon2::hash_raw(password.as_bytes(), &salt, &config)
            .map_err(|e| format!("Key derivation failed: {e}"))?;
        let mut key = Zeroizing::new([0u8; 32]);
        let ok = raw.len() == 32;
        if ok {
            key.copy_from_slice(&raw);
        }
        raw.zeroize();
        if !ok {
            return Err("Key derivation failed.".into());
        }
        Ok(VaultKey { key, salt, params })
    }

    pub fn params(&self) -> KdfParams {
        self.params
    }

    /// Whether `password` derives this key (slow).
    pub fn matches(&self, password: &str) -> bool {
        VaultKey::derive(password, self.salt, self.params)
            .is_ok_and(|k| k.key.as_slice() == self.key.as_slice())
    }
}

fn random(buf: &mut [u8]) -> Result<(), String> {
    getrandom::fill(buf).map_err(|_| "Could not get random bytes from the system.".to_string())
}

/// Encrypts `plaintext` into a file image with `magic`.
pub fn seal(magic: &[u8; 8], key: &VaultKey, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; NONCE];
    random(&mut nonce)?;
    let mut out = Vec::with_capacity(HEADER + plaintext.len() + TAG);
    out.extend_from_slice(magic);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(KDF_ARGON2ID);
    out.extend_from_slice(&key.params.memory_kib.to_le_bytes());
    out.extend_from_slice(&key.params.passes.to_le_bytes());
    out.extend_from_slice(&key.params.lanes.to_le_bytes());
    out.extend_from_slice(&key.salt);
    out.extend_from_slice(&nonce);
    let cipher = XChaCha20Poly1305::new_from_slice(key.key.as_slice())
        .map_err(|_| "Bad key length.".to_string())?;
    let sealed = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: &out,
            },
        )
        .map_err(|_| "Encryption failed.".to_string())?;
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// The header fields of a sealed file.
struct Header {
    params: KdfParams,
    salt: [u8; SALT],
    nonce: [u8; NONCE],
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn read_header(bytes: &[u8], magic: &[u8; 8]) -> Result<Header, String> {
    if bytes.len() < HEADER + TAG || &bytes[..8] != magic {
        return Err(if magic == VAULT_MAGIC {
            "This is not a BrowseCraft authenticator vault.".into()
        } else {
            "This is not a BrowseCraft authenticator backup.".into()
        });
    }
    let version = u16::from_le_bytes([bytes[8], bytes[9]]);
    if version == 0 {
        return Err("The file is damaged (format version 0).".into());
    }
    if version > VERSION {
        return Err(format!(
            "The file was made by a newer BrowseCraft (format {version}); update the browser to open it."
        ));
    }
    if bytes[10] != KDF_ARGON2ID {
        return Err("The file uses an unknown key derivation.".into());
    }
    let params = KdfParams {
        memory_kib: u32_at(bytes, 11),
        passes: u32_at(bytes, 15),
        lanes: u32_at(bytes, 19),
    };
    params.check()?;
    let mut salt = [0u8; SALT];
    salt.copy_from_slice(&bytes[23..23 + SALT]);
    let mut nonce = [0u8; NONCE];
    nonce.copy_from_slice(&bytes[39..39 + NONCE]);
    Ok(Header { params, salt, nonce })
}

/// Whether `bytes` start like a sealed file with `magic`.
pub fn has_magic(bytes: &[u8], magic: &[u8; 8]) -> bool {
    bytes.get(..8) == Some(magic.as_slice())
}

/// Derives the key from `password` and decrypts. Slow (key derivation).
pub fn unseal(bytes: &[u8], magic: &[u8; 8], password: &str) -> Result<(VaultKey, Zeroizing<Vec<u8>>), String> {
    let header = read_header(bytes, magic)?;
    let key = VaultKey::derive(password, header.salt, header.params)?;
    let plain = unseal_with(bytes, &key, &header)?;
    Ok((key, plain))
}

fn unseal_with(bytes: &[u8], key: &VaultKey, header: &Header) -> Result<Zeroizing<Vec<u8>>, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key.key.as_slice())
        .map_err(|_| "Bad key length.".to_string())?;
    cipher
        .decrypt(
            &XNonce::from(header.nonce),
            Payload {
                msg: &bytes[HEADER..],
                aad: &bytes[..HEADER],
            },
        )
        .map(Zeroizing::new)
        // A wrong password and a modified file look the same to an AEAD.
        .map_err(|_| "Wrong password, or the file was modified or damaged.".to_string())
}

/// Settings stored inside the vault.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Lock after this many seconds without use; 0 = never.
    pub auto_lock_secs: u64,
    /// Show codes only while the pointer is over them.
    pub hide_codes: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            auto_lock_secs: 5 * 60,
            hide_codes: false,
        }
    }
}

/// The decrypted contents.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Contents {
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub settings: Settings,
}

impl Contents {
    /// JSON in a buffer that is wiped when dropped. Sized up front so the
    /// writer does not reallocate and leave copies of the seeds behind.
    pub fn to_json(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        let mut buf = Zeroizing::new(Vec::with_capacity(4096 + self.accounts.len() * 1024));
        serde_json::to_writer(&mut *buf, self).map_err(|e| format!("Could not encode the vault: {e}"))?;
        Ok(buf)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Contents, String> {
        let mut contents: Contents =
            serde_json::from_slice(bytes).map_err(|e| format!("The vault contents are invalid: {e}"))?;
        for account in &mut contents.accounts {
            account.tidy();
        }
        contents.accounts.retain(|a| a.validate().is_ok());
        Ok(contents)
    }
}

/// An unlocked vault. Locking is dropping it (the key and seeds are wiped).
pub struct AuthVault {
    path: PathBuf,
    key: VaultKey,
    contents: Contents,
}

impl std::fmt::Debug for AuthVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthVault")
            .field("path", &self.path)
            .field("accounts", &self.contents.accounts.len())
            .finish()
    }
}

/// The default vault location in the per-user config folder.
pub fn default_path() -> Result<PathBuf, String> {
    crate::storage::config_file("authenticator.bcotp")
}

impl AuthVault {
    /// Creates an empty vault at `path`; never overwrites a file.
    pub fn create(path: &Path, password: &str) -> Result<AuthVault, String> {
        if path.exists() {
            return Err("An authenticator vault already exists.".into());
        }
        let key = VaultKey::new(password)?;
        let vault = AuthVault {
            path: path.to_path_buf(),
            key,
            contents: Contents::default(),
        };
        let bytes = seal(VAULT_MAGIC, &vault.key, &vault.contents.to_json()?)?;
        if let Some(dir) = path.parent() {
            crate::storage::private_dir(dir).map_err(|e| format!("Could not create the folder: {e}"))?;
        }
        crate::storage::create_new(path, &bytes, true).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => "An authenticator vault already exists.".to_string(),
            _ => format!("Could not save the vault: {e}"),
        })?;
        Ok(vault)
    }

    /// Opens and decrypts the vault at `path`.
    pub fn open(path: &Path, password: &str) -> Result<AuthVault, String> {
        let bytes = crate::storage::read_limited(path, MAX_FILE)
            .map_err(|e| format!("Could not read the vault: {e}"))?;
        let (key, plain) = unseal(&bytes, VAULT_MAGIC, password)?;
        let contents = Contents::from_json(&plain)?;
        Ok(AuthVault {
            path: path.to_path_buf(),
            key,
            contents,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn accounts(&self) -> &[Account] {
        &self.contents.accounts
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.contents.accounts.iter().find(|a| a.id == id)
    }

    pub fn settings(&self) -> &Settings {
        &self.contents.settings
    }

    /// All tags in use, sorted.
    pub fn tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = Vec::new();
        for tag in self.contents.accounts.iter().flat_map(|a| &a.tags) {
            if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                tags.push(tag.clone());
            }
        }
        tags.sort_by_key(|t| t.to_lowercase());
        tags
    }

    /// Applies `change` to a copy of the contents, writes it, and keeps it
    /// only if the write succeeded, so memory and disk never disagree.
    fn commit(&mut self, change: impl FnOnce(&mut Contents) -> Result<(), String>) -> Result<(), String> {
        let mut next = self.contents.clone();
        change(&mut next)?;
        let bytes = seal(VAULT_MAGIC, &self.key, &next.to_json()?)?;
        crate::storage::write_private(&self.path, &bytes).map_err(|e| format!("Could not save the vault: {e}"))?;
        self.contents = next;
        Ok(())
    }

    /// Adds an account at the end.
    pub fn add(&mut self, mut account: Account) -> Result<(), String> {
        account.tidy();
        account.validate()?;
        self.commit(|c| {
            c.accounts.push(account);
            Ok(())
        })
    }

    /// Adds the accounts not already present; returns how many were added.
    pub fn add_many(&mut self, accounts: Vec<Account>) -> Result<usize, String> {
        let mut added = 0;
        self.commit(|c| {
            for mut account in accounts {
                account.tidy();
                if account.validate().is_ok() && !c.accounts.iter().any(|a| a.same_as(&account)) {
                    if c.accounts.iter().any(|a| a.id == account.id) {
                        account.id = super::new_id();
                    }
                    c.accounts.push(account);
                    added += 1;
                }
            }
            Ok(())
        })?;
        Ok(added)
    }

    /// Replaces the account with the same id.
    pub fn update(&mut self, mut account: Account) -> Result<(), String> {
        account.tidy();
        account.validate()?;
        self.commit(|c| {
            let slot = c
                .accounts
                .iter_mut()
                .find(|a| a.id == account.id)
                .ok_or("That account no longer exists.")?;
            *slot = account;
            Ok(())
        })
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        self.commit(|c| {
            let before = c.accounts.len();
            c.accounts.retain(|a| a.id != id);
            if c.accounts.len() == before {
                return Err("That account no longer exists.".into());
            }
            Ok(())
        })
    }

    /// Moves an account one place up (`-1`) or down (`+1`).
    pub fn shift(&mut self, id: &str, delta: isize) -> Result<(), String> {
        self.commit(|c| {
            let i = c.accounts.iter().position(|a| a.id == id).ok_or("No such account.")?;
            let j = i.checked_add_signed(delta).filter(|&j| j < c.accounts.len());
            if let Some(j) = j {
                c.accounts.swap(i, j);
            }
            Ok(())
        })
    }

    /// Sorts the accounts by service, then account name.
    pub fn sort_by_name(&mut self) -> Result<(), String> {
        self.commit(|c| {
            c.accounts
                .sort_by_key(|a| (a.title().to_lowercase(), a.label.to_lowercase()));
            Ok(())
        })
    }

    /// Advances an HOTP account's counter and returns the new code.
    pub fn next_hotp(&mut self, id: &str) -> Result<String, String> {
        let mut code = String::new();
        self.commit(|c| {
            let a = c.accounts.iter_mut().find(|a| a.id == id).ok_or("No such account.")?;
            let super::Kind::Hotp { counter } = &mut a.kind else {
                return Err("Not a counter-based account.".into());
            };
            *counter = counter.checked_add(1).ok_or("The counter is exhausted.")?;
            code = a.code_at(0);
            Ok(())
        })?;
        Ok(code)
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<(), String> {
        self.commit(|c| {
            c.settings = settings;
            Ok(())
        })
    }

    /// Whether `password` is the current master password (slow).
    pub fn check_password(&self, password: &str) -> bool {
        self.key.matches(password)
    }

    /// The current key, to check the old password off the UI thread.
    pub fn key(&self) -> &VaultKey {
        &self.key
    }

    /// Re-encrypts the vault under `key` (from [`VaultKey::new`] on the new
    /// password). The old key is wiped on success.
    pub fn change_key(&mut self, key: VaultKey) -> Result<(), String> {
        let bytes = seal(VAULT_MAGIC, &key, &self.contents.to_json()?)?;
        crate::storage::write_private(&self.path, &bytes).map_err(|e| format!("Could not save the vault: {e}"))?;
        self.key = key;
        Ok(())
    }

    /// A key-free copy of what an export needs.
    pub fn snapshot(&self) -> Vec<Account> {
        self.contents.accounts.clone()
    }
}

/// Encrypts `accounts` as a backup file image under `password` (slow).
pub fn backup_bytes(accounts: &[Account], password: &str) -> Result<Vec<u8>, String> {
    let key = VaultKey::new(password)?;
    let contents = Contents {
        accounts: accounts.to_vec(),
        settings: Settings::default(),
    };
    seal(BACKUP_MAGIC, &key, &contents.to_json()?)
}

/// Decrypts a backup file image (slow).
pub fn restore_backup(bytes: &[u8], password: &str) -> Result<Vec<Account>, String> {
    let (_, plain) = unseal(bytes, BACKUP_MAGIC, password)?;
    Ok(Contents::from_json(&plain)?.accounts)
}

#[cfg(test)]
mod tests {
    use super::super::{Kind, Secret};
    use super::*;

    const PW: &str = "correct horse battery";

    fn account(name: &str) -> Account {
        Account::totp(name, "me@example.com", Secret::from_base32("JBSWY3DPEHPK3PXP").unwrap())
    }

    fn temp_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault").join("auth.bcotp");
        (dir, path)
    }

    #[test]
    fn create_save_reopen() {
        let (_dir, path) = temp_path();
        let mut v = AuthVault::create(&path, PW).unwrap();
        v.add(account("GitHub")).unwrap();
        v.add(account("GitLab")).unwrap();
        let mut s = v.settings().clone();
        s.auto_lock_secs = 60;
        v.set_settings(s).unwrap();
        drop(v);
        let raw = std::fs::read(&path).unwrap();
        assert!(has_magic(&raw, VAULT_MAGIC));
        assert!(!raw.windows(16).any(|w| w == b"JBSWY3DPEHPK3PXP"), "seed in clear");
        assert!(!raw.windows(6).any(|w| w == b"GitHub"), "names in clear");
        let v = AuthVault::open(&path, PW).unwrap();
        let names: Vec<_> = v.accounts().iter().map(|a| a.issuer.as_str()).collect();
        assert_eq!(names, ["GitHub", "GitLab"]);
        assert_eq!(v.settings().auto_lock_secs, 60);
        assert!(AuthVault::create(&path, PW).is_err(), "never overwrites");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "owner-only file");
        }
    }

    #[test]
    fn wrong_password_and_short_password() {
        let (_dir, path) = temp_path();
        assert!(AuthVault::create(&path, "short").is_err());
        AuthVault::create(&path, PW).unwrap();
        let err = AuthVault::open(&path, "correct horse battery!").unwrap_err();
        assert!(err.contains("Wrong password"), "{err}");
    }

    #[test]
    fn tampering_is_detected_anywhere() {
        let key = VaultKey::new(PW).unwrap();
        let sealed = seal(VAULT_MAGIC, &key, b"{\"accounts\":[]}").unwrap();
        assert!(unseal(&sealed, VAULT_MAGIC, PW).is_ok());
        // Flip one bit in the salt, the nonce, the ciphertext and the tag.
        for at in [30, 50, HEADER + 2, sealed.len() - 1] {
            let mut bad = sealed.clone();
            bad[at] ^= 1;
            assert!(unseal(&bad, VAULT_MAGIC, PW).is_err(), "byte {at}");
        }
        // Weaker parameters (still in range) are part of the AAD too.
        let mut bad = sealed.clone();
        bad[11..15].copy_from_slice(&128u32.to_le_bytes());
        assert!(unseal(&bad, VAULT_MAGIC, PW).is_err());
        // Truncation.
        assert!(unseal(&sealed[..sealed.len() - 3], VAULT_MAGIC, PW).is_err());
        assert!(unseal(&sealed[..20], VAULT_MAGIC, PW).is_err());
    }

    #[test]
    fn version_and_parameter_handling() {
        let key = VaultKey::new(PW).unwrap();
        let sealed = seal(VAULT_MAGIC, &key, b"{}").unwrap();
        let mut newer = sealed.clone();
        newer[8..10].copy_from_slice(&2u16.to_le_bytes());
        assert!(unseal(&newer, VAULT_MAGIC, PW).unwrap_err().contains("newer"));
        let mut zero = sealed.clone();
        zero[8..10].copy_from_slice(&0u16.to_le_bytes());
        assert!(unseal(&zero, VAULT_MAGIC, PW).is_err());
        let mut kdf = sealed.clone();
        kdf[10] = 9;
        assert!(unseal(&kdf, VAULT_MAGIC, PW).unwrap_err().contains("unknown"));
        // A 4 GiB memory request is refused before any allocation.
        let mut huge = sealed.clone();
        huge[11..15].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(unseal(&huge, VAULT_MAGIC, PW).unwrap_err().contains("range"));
        // A backup is not a vault and the other way round.
        assert!(unseal(&sealed, BACKUP_MAGIC, PW).is_err());
        // Strong defaults are what production writes.
        assert_eq!(KdfParams::STRONG.memory_kib, 65536);
        assert!(KdfParams::STRONG.check().is_ok());
    }

    #[test]
    fn edits_order_and_hotp() {
        let (_dir, path) = temp_path();
        let mut v = AuthVault::create(&path, PW).unwrap();
        v.add(account("b")).unwrap();
        v.add(account("a")).unwrap();
        let mut h = account("c");
        h.kind = Kind::Hotp { counter: 0 };
        h.secret = Secret::from_slice(b"12345678901234567890");
        let hid = h.id.clone();
        v.add(h).unwrap();
        v.sort_by_name().unwrap();
        let order = |v: &AuthVault| v.accounts().iter().map(|a| a.issuer.clone()).collect::<Vec<_>>();
        assert_eq!(order(&v), ["a", "b", "c"]);
        v.shift(&hid, -1).unwrap();
        assert_eq!(order(&v), ["a", "c", "b"]);
        v.shift(&v.accounts()[0].id.clone(), -1).unwrap();
        assert_eq!(order(&v), ["a", "c", "b"], "top stays top");
        assert_eq!(v.next_hotp(&hid).unwrap(), "287082");
        let mut edited = v.account(&hid).unwrap().clone();
        edited.tags = vec!["work".into()];
        v.update(edited).unwrap();
        assert_eq!(v.tags(), ["work"]);
        let first = v.accounts()[0].id.clone();
        v.remove(&first).unwrap();
        assert!(v.remove(&first).is_err());
        let v = AuthVault::open(&path, PW).unwrap();
        assert_eq!(order(&v), ["c", "b"]);
        assert_eq!(v.account(&hid).unwrap().kind, Kind::Hotp { counter: 1 });
        // Duplicates are skipped on bulk add.
        let mut v = v;
        let dup = v.accounts()[1].clone();
        assert_eq!(v.add_many(vec![dup, account("new")]).unwrap(), 1);
    }

    #[test]
    fn failed_save_keeps_memory_unchanged() {
        let (_dir, path) = temp_path();
        let mut v = AuthVault::create(&path, PW).unwrap();
        // Replace the folder with a file so the write fails.
        let dir = path.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"x").unwrap();
        assert!(v.add(account("x")).is_err());
        assert!(v.accounts().is_empty());
    }

    #[test]
    fn change_password() {
        let (_dir, path) = temp_path();
        let mut v = AuthVault::create(&path, PW).unwrap();
        v.add(account("x")).unwrap();
        assert!(v.check_password(PW));
        assert!(!v.check_password("nope nope nope"));
        v.change_key(VaultKey::new("a brand new password").unwrap()).unwrap();
        assert!(AuthVault::open(&path, PW).is_err());
        assert_eq!(AuthVault::open(&path, "a brand new password").unwrap().accounts().len(), 1);
    }

    #[test]
    fn backup_round_trip() {
        let mut a = account("x");
        a.tags = vec!["t".into()];
        let bytes = backup_bytes(std::slice::from_ref(&a), "backup password").unwrap();
        assert!(has_magic(&bytes, BACKUP_MAGIC));
        let back = restore_backup(&bytes, "backup password").unwrap();
        assert_eq!(back, vec![a]);
        assert!(restore_backup(&bytes, "wrong password!").is_err());
    }

    #[test]
    fn debug_is_redacted() {
        let key = VaultKey::new(PW).unwrap();
        let text = format!("{key:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains(&format!("{:?}", key.key.as_slice())));
    }
}
