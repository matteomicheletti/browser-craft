//! The authenticator: TOTP/HOTP codes for two-factor sign-in, kept in an
//! encrypted vault of its own (see `docs/AUTHENTICATOR.md` for the format and
//! why it is separate from the KeePass password vault).
//!
//! * [`otp`]: the RFC 4226 / 6238 algorithms and base32.
//! * [`uri`]: `otpauth://` URIs and Google Authenticator export QR codes.
//! * [`vault`]: the encrypted file (Argon2id + XChaCha20-Poly1305).
//! * [`import`]: backups, URI lists, Aegis files and QR images.
//! * [`panel`]: the drawer panel.
//!
//! Seeds never leave this module except into the vault file and the user's
//! explicit exports: pages, plugins and the automation API get nothing.

// The public API is wired into the toolbar, palette and drawer in the
// integration step; until then most of it is only reached from tests.
#![allow(dead_code)]

pub mod import;
pub mod otp;
pub mod panel;
pub mod uri;
pub mod vault;

#[allow(unused_imports)] // wired in integration
pub use panel::{AuthEvent, AuthenticatorPanel};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use otp::Algorithm;

/// A shared secret (the TOTP seed). Wiped when dropped; never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<Vec<u8>>);

/// Longest seed accepted. Real ones are 10–64 bytes.
pub const MAX_SECRET: usize = 256;

impl Secret {
    pub fn new(bytes: Zeroizing<Vec<u8>>) -> Secret {
        Secret(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Secret {
        Secret(Zeroizing::new(bytes.to_vec()))
    }

    /// Parses a base32 secret as typed or pasted (spaces, any case).
    pub fn from_base32(text: &str) -> Option<Secret> {
        otp::base32_decode(text).map(Secret)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_base32(&self) -> Zeroizing<String> {
        otp::base32_encode(&self.0)
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_base32())
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Secret;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a base32 secret")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Secret, E> {
                Secret::from_base32(v).ok_or_else(|| E::custom("invalid base32 secret"))
            }
        }
        d.deserialize_str(Visitor)
    }
}

/// Time-based or counter-based codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kind {
    Totp { period: u32 },
    Hotp { counter: u64 },
}

/// One account in the authenticator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Random, stable id (32 hex digits).
    pub id: String,
    /// The service (`GitHub`).
    pub issuer: String,
    /// The account at that service (`alice@example.com`).
    pub label: String,
    pub secret: Secret,
    #[serde(default)]
    pub algorithm: Algorithm,
    pub digits: u32,
    pub kind: Kind,
    /// Groups the user put the account in, for filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub note: String,
}

/// Longest issuer, label, tag or note kept.
pub const MAX_TEXT: usize = 512;

impl Account {
    /// A TOTP account with the usual settings (SHA-1, 6 digits, 30 s).
    pub fn totp(issuer: &str, label: &str, secret: Secret) -> Account {
        Account {
            id: new_id(),
            issuer: issuer.to_string(),
            label: label.to_string(),
            secret,
            algorithm: Algorithm::Sha1,
            digits: 6,
            kind: Kind::Totp { period: 30 },
            tags: Vec::new(),
            note: String::new(),
        }
    }

    /// Checks the parameters are ones codes can be made with.
    pub fn validate(&self) -> Result<(), String> {
        if self.secret.bytes().is_empty() {
            return Err("The secret is empty.".into());
        }
        if self.secret.bytes().len() > MAX_SECRET {
            return Err("The secret is too long.".into());
        }
        if !(otp::MIN_DIGITS..=otp::MAX_DIGITS).contains(&self.digits) {
            return Err(format!(
                "Codes must have {} to {} digits.",
                otp::MIN_DIGITS,
                otp::MAX_DIGITS
            ));
        }
        if let Kind::Totp { period } = self.kind
            && !(otp::MIN_PERIOD..=otp::MAX_PERIOD).contains(&period)
        {
            return Err(format!(
                "The period must be {} to {} seconds.",
                otp::MIN_PERIOD,
                otp::MAX_PERIOD
            ));
        }
        if self.issuer.trim().is_empty() && self.label.trim().is_empty() {
            return Err("Give the account a service or account name.".into());
        }
        let long = [&self.issuer, &self.label, &self.note]
            .into_iter()
            .chain(self.tags.iter())
            .any(|t| t.len() > MAX_TEXT);
        if long {
            return Err("A name, tag or note is too long.".into());
        }
        Ok(())
    }

    /// The code to show now: the TOTP code at `unix_time`, or the HOTP code
    /// for the current counter.
    pub fn code_at(&self, unix_time: u64) -> String {
        let value = match self.kind {
            Kind::Totp { period } => {
                otp::totp(self.secret.bytes(), unix_time, period, self.algorithm, self.digits)
            }
            Kind::Hotp { counter } => {
                otp::hotp(self.secret.bytes(), counter, self.algorithm, self.digits)
            }
        };
        otp::format_code(value, self.digits)
    }

    /// The name shown in lists: issuer, else label.
    pub fn title(&self) -> &str {
        if self.issuer.trim().is_empty() {
            &self.label
        } else {
            &self.issuer
        }
    }

    /// Whether a search query matches (case-insensitive, every word must
    /// appear in the issuer, label, tags or note).
    pub fn matches(&self, query: &str) -> bool {
        let hay = format!(
            "{} {} {} {}",
            self.issuer,
            self.label,
            self.tags.join(" "),
            self.note
        )
        .to_lowercase();
        query
            .split_whitespace()
            .all(|word| hay.contains(&word.to_lowercase()))
    }

    /// The same seed for the same service and account: imports skip these.
    pub fn same_as(&self, other: &Account) -> bool {
        self.secret == other.secret
            && self.issuer.eq_ignore_ascii_case(&other.issuer)
            && self.label == other.label
            && std::mem::discriminant(&self.kind) == std::mem::discriminant(&other.kind)
    }

    /// Trims names, drops empty or repeated tags, caps lengths.
    pub fn tidy(&mut self) {
        fn cap(s: &str) -> String {
            let s = s.trim();
            if s.len() <= MAX_TEXT {
                return s.to_string();
            }
            let mut end = MAX_TEXT;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s[..end].to_string()
        }
        self.issuer = cap(&self.issuer);
        self.label = cap(&self.label);
        self.note = cap(&self.note);
        let mut tags: Vec<String> = Vec::new();
        for tag in &self.tags {
            let tag = cap(tag);
            if !tag.is_empty() && !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                tags.push(tag);
            }
        }
        self.tags = tags;
    }
}

/// A random 128-bit id in hex.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    // Without system randomness the vault cannot be saved either (its nonce
    // needs it), so a fallback id only has to be unique in this process.
    if getrandom::fill(&mut bytes).is_err() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        bytes[..8].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        bytes[8..].copy_from_slice(&otp::now().to_be_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Account {
        Account::totp("GitHub", "alice@example.com", Secret::from_base32("JBSWY3DPEHPK3PXP").unwrap())
    }

    #[test]
    fn debug_output_never_shows_the_secret() {
        let account = sample();
        let text = format!("{account:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("JBSWY3DPEHPK3PXP"));
        assert!(!text.contains("72, 101"), "raw bytes of the seed");
    }

    #[test]
    fn serde_round_trip() {
        let mut account = sample();
        account.tags = vec!["work".into()];
        account.kind = Kind::Hotp { counter: 7 };
        let json = serde_json::to_string(&account).unwrap();
        assert!(json.contains("\"secret\":\"JBSWY3DPEHPK3PXP\""));
        let back: Account = serde_json::from_str(&json).unwrap();
        assert_eq!(back, account);
    }

    #[test]
    fn validation() {
        let mut a = sample();
        assert!(a.validate().is_ok());
        a.digits = 5;
        assert!(a.validate().is_err());
        a.digits = 8;
        a.kind = Kind::Totp { period: 0 };
        assert!(a.validate().is_err());
        a.kind = Kind::Totp { period: 60 };
        a.issuer.clear();
        a.label.clear();
        assert!(a.validate().is_err());
    }

    #[test]
    fn search_and_tidy() {
        let mut a = sample();
        a.tags = vec![" Work ".into(), "work".into(), "".into()];
        a.tidy();
        assert_eq!(a.tags, vec!["Work".to_string()]);
        assert!(a.matches("git alice"));
        assert!(a.matches("WORK"));
        assert!(!a.matches("gitlab"));
        assert!(a.matches(""));
    }

    #[test]
    fn codes_for_both_kinds() {
        let mut a = Account::totp("x", "y", Secret::from_slice(b"12345678901234567890"));
        a.digits = 8;
        assert_eq!(a.code_at(59), "94287082");
        a.digits = 6;
        a.kind = Kind::Hotp { counter: 1 };
        assert_eq!(a.code_at(0), "287082");
    }
}
