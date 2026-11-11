//! `otpauth://` URIs (Google's Key Uri Format, what setup QR codes hold) and
//! `otpauth-migration://` URIs (Google Authenticator's "Transfer accounts"
//! QR codes, a base64 protobuf).
//!
//! Key Uri Format: `otpauth://TYPE/LABEL?secret=…&issuer=…&algorithm=…&
//! digits=…&period=…|counter=…`, where LABEL is `issuer:account` or
//! `account`, percent-encoded.

use base64::Engine as _;
use zeroize::Zeroizing;

use super::otp::Algorithm;
use super::{Account, Kind, Secret};

/// Longest `otpauth://` URI accepted (a QR code holds at most ~3 KB).
pub const MAX_URI: usize = 4096;
/// Longest migration URI accepted.
pub const MAX_MIGRATION_URI: usize = 64 * 1024;

/// Parses an `otpauth://` URI into a new account (fresh id, no tags).
pub fn parse(uri: &str) -> Result<Account, String> {
    let uri = uri.trim();
    if uri.len() > MAX_URI {
        return Err("The link is too long.".into());
    }
    let rest = strip_prefix_ci(uri, "otpauth://").ok_or("Not an otpauth:// link.")?;
    let (kind_text, rest) = rest.split_once('/').ok_or("The link has no account name.")?;
    let (label_raw, query) = rest.split_once('?').unwrap_or((rest, ""));
    // Split on a literal colon before decoding, so an encoded colon inside
    // the issuer (`ACME%3A%20Ltd:bob`) stays part of it; a label with only
    // an encoded colon (`ACME%3Abob`, as the spec allows) is split after.
    let decode = |s: &str| percent_decode(s).ok_or("The account name is not valid UTF-8.");
    let label = match label_raw.split_once(':') {
        Some((issuer, account)) => (Some(decode(issuer)?), decode(account)?),
        None => {
            let label = decode(label_raw)?;
            match label.split_once(':') {
                Some((i, a)) => (Some(i.to_string()), a.to_string()),
                None => (None, label),
            }
        }
    };

    let mut secret: Option<Secret> = None;
    let mut issuer_param: Option<String> = None;
    let mut algorithm = Algorithm::Sha1;
    let mut digits = 6u32;
    let mut period = 30u32;
    let mut counter: Option<u64> = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let value = Zeroizing::new(value.into_owned());
        match key.to_ascii_lowercase().as_str() {
            "secret" => {
                secret = Some(Secret::from_base32(&value).ok_or("The secret is not valid base32.")?)
            }
            "issuer" => issuer_param = Some(value.trim().to_string()),
            "algorithm" => {
                algorithm = Algorithm::parse(&value)
                    .ok_or_else(|| format!("Unsupported algorithm {}.", value.as_str()))?
            }
            "digits" => digits = value.trim().parse().map_err(|_| "Invalid digits.")?,
            "period" => period = value.trim().parse().map_err(|_| "Invalid period.")?,
            "counter" => counter = Some(value.trim().parse().map_err(|_| "Invalid counter.")?),
            _ => {} // image=, color= and other vendor extras are ignored
        }
    }
    let kind = match kind_text.to_ascii_lowercase().as_str() {
        "totp" => Kind::Totp { period },
        "hotp" => Kind::Hotp {
            counter: counter.ok_or("An HOTP link needs a counter.")?,
        },
        other => return Err(format!("Unsupported code type \"{other}\".")),
    };
    let (issuer, label) = split_label(label, issuer_param);
    let mut account = Account {
        id: super::new_id(),
        issuer,
        label,
        secret: secret.ok_or("The link has no secret.")?,
        algorithm,
        digits,
        kind,
        tags: Vec::new(),
        note: String::new(),
    };
    account.tidy();
    account.validate()?;
    Ok(account)
}

/// Reconciles a label's issuer prefix with the `issuer` parameter, which
/// wins when both are there (the spec says they should agree).
fn split_label((prefix, account): (Option<String>, String), issuer_param: Option<String>) -> (String, String) {
    let issuer = issuer_param
        .filter(|i| !i.trim().is_empty())
        .or(prefix)
        .unwrap_or_default();
    (issuer.trim().to_string(), account.trim().to_string())
}

/// `Issuer:account` or `account`, split on the first colon.
fn split_name(name: &str) -> (Option<String>, String) {
    match name.split_once(':') {
        Some((p, a)) => (Some(p.to_string()), a.to_string()),
        None => (None, name.to_string()),
    }
}

/// The `otpauth://` URI for `account` (the secret is in it: callers only
/// show or write it on the user's explicit request).
pub fn to_uri(account: &Account) -> Zeroizing<String> {
    let kind = match account.kind {
        Kind::Totp { .. } => "totp",
        Kind::Hotp { .. } => "hotp",
    };
    let label = if account.issuer.is_empty() {
        percent_encode(&account.label)
    } else {
        format!("{}:{}", percent_encode(&account.issuer), percent_encode(&account.label))
    };
    let mut out = Zeroizing::new(format!("otpauth://{kind}/{label}?secret="));
    out.push_str(&account.secret.to_base32());
    if !account.issuer.is_empty() {
        out.push_str("&issuer=");
        out.push_str(&percent_encode(&account.issuer));
    }
    out.push_str("&algorithm=");
    out.push_str(account.algorithm.name());
    out.push_str(&format!("&digits={}", account.digits));
    match account.kind {
        Kind::Totp { period } => out.push_str(&format!("&period={period}")),
        Kind::Hotp { counter } => out.push_str(&format!("&counter={counter}")),
    }
    out
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

/// Percent-decodes a path segment (`+` stays a plus, unlike in queries).
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Percent-encodes everything but unreserved characters and `@`, so spaces
/// become `%20` and a colon in a name cannot be read as the separator.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What a Google Authenticator export QR code held.
#[derive(Debug)]
pub struct Migration {
    pub accounts: Vec<Account>,
    /// Entries that cannot be used (MD5, unknown type, bad parameters).
    pub skipped: usize,
    /// "QR code n of m" (0-based index), when the export spans several.
    pub batch_index: u64,
    pub batch_size: u64,
}

/// Parses `otpauth-migration://offline?data=…`.
///
/// The payload is the `MigrationPayload` protobuf of Google Authenticator:
/// field 1 repeated `OtpParameters` {1 secret bytes, 2 name, 3 issuer,
/// 4 algorithm (1 SHA1, 2 SHA256, 3 SHA512, 4 MD5), 5 digits (1 six,
/// 2 eight), 6 type (1 HOTP, 2 TOTP), 7 counter}, 2 version, 3 batch size,
/// 4 batch index, 5 batch id.
pub fn parse_migration(uri: &str) -> Result<Migration, String> {
    let uri = uri.trim();
    if uri.len() > MAX_MIGRATION_URI {
        return Err("The link is too long.".into());
    }
    let rest = strip_prefix_ci(uri, "otpauth-migration://")
        .ok_or("Not an otpauth-migration:// link.")?;
    let query = rest.split_once('?').map(|(_, q)| q).unwrap_or("");
    let data = url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "data")
        .map(|(_, v)| Zeroizing::new(v.replace(' ', "+")))
        .ok_or("The link has no data.")?;
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let bytes = Zeroizing::new(engine.decode(data.as_bytes()).map_err(|_| "The data is not valid base64.")?);
    let mut out = Migration {
        accounts: Vec::new(),
        skipped: 0,
        batch_index: 0,
        batch_size: 1,
    };
    for field in Fields::new(&bytes) {
        match field? {
            (1, Value::Bytes(entry)) => match parse_otp_parameters(entry) {
                Ok(account) => out.accounts.push(account),
                Err(_) => out.skipped += 1,
            },
            (3, Value::Varint(n)) => out.batch_size = n,
            (4, Value::Varint(n)) => out.batch_index = n,
            _ => {}
        }
    }
    Ok(out)
}

fn parse_otp_parameters(bytes: &[u8]) -> Result<Account, String> {
    let mut secret = None;
    let (mut name, mut issuer) = (String::new(), String::new());
    let (mut algorithm, mut digits, mut kind, mut counter) = (Algorithm::Sha1, 6, 2, 0u64);
    for field in Fields::new(bytes) {
        match field? {
            (1, Value::Bytes(b)) => secret = Some(Secret::from_slice(b)),
            (2, Value::Bytes(b)) => name = String::from_utf8_lossy(b).into_owned(),
            (3, Value::Bytes(b)) => issuer = String::from_utf8_lossy(b).into_owned(),
            (4, Value::Varint(n)) => {
                algorithm = match n {
                    0 | 1 => Algorithm::Sha1,
                    2 => Algorithm::Sha256,
                    3 => Algorithm::Sha512,
                    _ => return Err("Unsupported algorithm.".into()),
                }
            }
            (5, Value::Varint(n)) => {
                digits = match n {
                    0 | 1 => 6,
                    2 => 8,
                    _ => return Err("Unsupported digits.".into()),
                }
            }
            (6, Value::Varint(n)) => kind = n,
            (7, Value::Varint(n)) => counter = n,
            _ => {}
        }
    }
    let kind = match kind {
        1 => Kind::Hotp { counter },
        0 | 2 => Kind::Totp { period: 30 },
        _ => return Err("Unsupported type.".into()),
    };
    let (issuer, label) = split_label(split_name(&name), Some(issuer));
    let mut account = Account {
        id: super::new_id(),
        issuer,
        label,
        secret: secret.ok_or("No secret.")?,
        algorithm,
        digits,
        kind,
        tags: Vec::new(),
        note: String::new(),
    };
    account.tidy();
    account.validate()?;
    Ok(account)
}

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// A minimal protobuf reader: (field number, value) pairs, bounds-checked.
struct Fields<'a> {
    data: &'a [u8],
    failed: bool,
}

impl<'a> Fields<'a> {
    fn new(data: &'a [u8]) -> Self {
        Fields { data, failed: false }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = self.data.split_first()?;
            self.data = rest;
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.data.len() {
            return None;
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Some(head)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.data.is_empty() {
            return None;
        }
        let item = (|| {
            let key = self.varint()?;
            let value = match key & 7 {
                0 => Value::Varint(self.varint()?),
                1 => {
                    self.take(8)?;
                    Value::Fixed
                }
                2 => {
                    let len = usize::try_from(self.varint()?).ok()?;
                    Value::Bytes(self.take(len)?)
                }
                5 => {
                    self.take(4)?;
                    Value::Fixed
                }
                _ => return None,
            };
            Some((key >> 3, value))
        })();
        match item {
            Some(item) => Some(Ok(item)),
            None => {
                self.failed = true;
                Some(Err("The export data is damaged.".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_key_uri_format_example() {
        let a = parse("otpauth://totp/Example:alice@google.com?secret=JBSWY3DPEHPK3PXP&issuer=Example")
            .unwrap();
        assert_eq!(a.issuer, "Example");
        assert_eq!(a.label, "alice@google.com");
        assert_eq!(a.secret.bytes(), b"Hello!\xde\xad\xbe\xef");
        assert_eq!((a.algorithm, a.digits, a.kind), (Algorithm::Sha1, 6, Kind::Totp { period: 30 }));
    }

    #[test]
    fn label_and_issuer_rules() {
        // Encoded colon and space after it; issuer only in the label.
        let a = parse("otpauth://totp/ACME%20Co%3A%20john.doe%40email.com?secret=JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!((a.issuer.as_str(), a.label.as_str()), ("ACME Co", "john.doe@email.com"));
        // The parameter wins over the prefix.
        let a = parse("otpauth://totp/Old:bob?secret=JBSWY3DPEHPK3PXP&issuer=New").unwrap();
        assert_eq!((a.issuer.as_str(), a.label.as_str()), ("New", "bob"));
        // No issuer at all.
        let a = parse("otpauth://totp/bob?secret=jbsw%20y3dp%20ehpk%203pxp").unwrap();
        assert_eq!((a.issuer.as_str(), a.label.as_str()), ("", "bob"));
        assert_eq!(a.secret.bytes(), b"Hello!\xde\xad\xbe\xef");
        // A plus in the path is a plus.
        let a = parse("otpauth://totp/a+b?secret=JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(a.label, "a+b");
    }

    #[test]
    fn all_parameters_and_hotp() {
        let a = parse(
            "OTPAUTH://HOTP/Svc:me?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&algorithm=sha256&digits=8&counter=42",
        )
        .unwrap();
        assert_eq!(a.kind, Kind::Hotp { counter: 42 });
        assert_eq!((a.algorithm, a.digits), (Algorithm::Sha256, 8));
        let a = parse("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=60&algorithm=SHA512").unwrap();
        assert_eq!((a.kind, a.algorithm), (Kind::Totp { period: 60 }, Algorithm::Sha512));
    }

    #[test]
    fn rejects_bad_uris() {
        for bad in [
            "https://example.com",
            "otpauth://totp/x",
            "otpauth://totp/x?secret=not*base32",
            "otpauth://hotp/x?secret=JBSWY3DPEHPK3PXP",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=10",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=MD5",
            "otpauth://motp/x?secret=JBSWY3DPEHPK3PXP",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=0",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let long = format!("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&n={}", "a".repeat(MAX_URI));
        assert!(parse(&long).is_err());
    }

    #[test]
    fn round_trips() {
        let mut a = Account::totp("ACME Co: Ltd", "john doe@x.com", Secret::from_slice(b"0123456789abcdefghij"));
        a.algorithm = Algorithm::Sha512;
        a.digits = 7;
        a.kind = Kind::Totp { period: 45 };
        let uri = to_uri(&a);
        assert!(uri.starts_with("otpauth://totp/ACME%20Co%3A%20Ltd:john%20doe@x.com?secret="));
        let b = parse(&uri).unwrap();
        assert!(a.same_as(&b));
        assert_eq!((b.algorithm, b.digits, b.kind), (a.algorithm, a.digits, a.kind));

        let mut h = Account::totp("", "only label", Secret::from_slice(b"abc"));
        h.kind = Kind::Hotp { counter: 9 };
        let back = parse(&to_uri(&h)).unwrap();
        assert!(h.same_as(&back));
        assert_eq!(back.kind, Kind::Hotp { counter: 9 });
    }

    #[test]
    fn google_migration_sample() {
        // A Google Authenticator export of the Key Uri Format example account.
        let uri = "otpauth-migration://offline?data=CjEKCkhlbGxvId6tvu8SGEV4YW1wbGU6YWxpY2VAZ29vZ2xlLmNvbRoHRXhhbXBsZSABKAEwAhABGAEgACjr4JKrBA%3D%3D";
        let m = parse_migration(uri).unwrap();
        assert_eq!(m.accounts.len(), 1);
        assert_eq!((m.skipped, m.batch_index, m.batch_size), (0, 0, 1));
        let a = &m.accounts[0];
        assert_eq!((a.issuer.as_str(), a.label.as_str()), ("Example", "alice@google.com"));
        assert_eq!(a.secret.to_base32().as_str(), "JBSWY3DPEHPK3PXP");
        assert_eq!((a.algorithm, a.digits, a.kind), (Algorithm::Sha1, 6, Kind::Totp { period: 30 }));
    }

    /// Encodes a protobuf field (test helper, mirrors the reader).
    fn field(out: &mut Vec<u8>, number: u64, value: Result<u64, &[u8]>) {
        fn varint(out: &mut Vec<u8>, mut v: u64) {
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(b);
                    return;
                }
                out.push(b | 0x80);
            }
        }
        match value {
            Ok(v) => {
                varint(out, number << 3);
                varint(out, v);
            }
            Err(bytes) => {
                varint(out, (number << 3) | 2);
                varint(out, bytes.len() as u64);
                out.extend_from_slice(bytes);
            }
        }
    }

    #[test]
    fn google_migration_hotp_sha256_eight_digits_and_md5_skipped() {
        let mut hotp = Vec::new();
        field(&mut hotp, 1, Err(b"12345678901234567890"));
        field(&mut hotp, 2, Err(b"bob"));
        field(&mut hotp, 3, Err(b"Svc"));
        field(&mut hotp, 4, Ok(2));
        field(&mut hotp, 5, Ok(2));
        field(&mut hotp, 6, Ok(1));
        field(&mut hotp, 7, Ok(5));
        let mut md5 = Vec::new();
        field(&mut md5, 1, Err(b"abc"));
        field(&mut md5, 2, Err(b"m"));
        field(&mut md5, 4, Ok(4));
        let mut payload = Vec::new();
        field(&mut payload, 1, Err(&hotp));
        field(&mut payload, 1, Err(&md5));
        field(&mut payload, 2, Ok(1));
        field(&mut payload, 3, Ok(2));
        field(&mut payload, 4, Ok(1));
        let data = base64::engine::general_purpose::STANDARD.encode(&payload);
        let uri = format!(
            "otpauth-migration://offline?data={}",
            url::form_urlencoded::byte_serialize(data.as_bytes()).collect::<String>()
        );
        let m = parse_migration(&uri).unwrap();
        assert_eq!((m.accounts.len(), m.skipped, m.batch_index, m.batch_size), (1, 1, 1, 2));
        let a = &m.accounts[0];
        assert_eq!((a.issuer.as_str(), a.label.as_str()), ("Svc", "bob"));
        assert_eq!((a.algorithm, a.digits, a.kind), (Algorithm::Sha256, 8, Kind::Hotp { counter: 5 }));

        // Truncated data is reported, not panicked on.
        let cut = base64::engine::general_purpose::STANDARD.encode(&payload[..payload.len() / 2]);
        assert!(parse_migration(&format!("otpauth-migration://offline?data={cut}")).is_err());
        assert!(parse_migration("otpauth-migration://offline?data=%%%").is_err());
    }
}
