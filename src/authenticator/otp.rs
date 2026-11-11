//! One-time passwords: HOTP (RFC 4226) and TOTP (RFC 6238), and the base32
//! encoding (RFC 4648) their secrets are shared in.

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// The HMAC hash. SHA-1 is what nearly every site uses; SHA-256/512 are in
/// RFC 6238 and supported by most apps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Algorithm {
    #[default]
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    pub const ALL: [Algorithm; 3] = [Algorithm::Sha1, Algorithm::Sha256, Algorithm::Sha512];

    /// The name used in `otpauth://` URIs and Aegis files.
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Sha1 => "SHA1",
            Algorithm::Sha256 => "SHA256",
            Algorithm::Sha512 => "SHA512",
        }
    }

    /// Parses `SHA1`, `sha-256`, `SHA512`… (case and dash tolerant).
    pub fn parse(text: &str) -> Option<Algorithm> {
        match text.to_ascii_uppercase().replace('-', "").as_str() {
            "SHA1" => Some(Algorithm::Sha1),
            "SHA256" => Some(Algorithm::Sha256),
            "SHA512" => Some(Algorithm::Sha512),
            _ => None,
        }
    }
}

/// Shortest and longest codes accepted. RFC 4226 requires at least 6; more
/// than 8 is not used by any service and overflows nothing but readability.
pub const MIN_DIGITS: u32 = 6;
pub const MAX_DIGITS: u32 = 8;
/// TOTP step limits in seconds (30 is the norm, some use 60).
pub const MIN_PERIOD: u32 = 1;
pub const MAX_PERIOD: u32 = 3600;

/// HOTP value for `counter` (RFC 4226 section 5.3), `digits` long.
pub fn hotp(secret: &[u8], counter: u64, algorithm: Algorithm, digits: u32) -> u32 {
    let msg = counter.to_be_bytes();
    let digest: Zeroizing<Vec<u8>> = Zeroizing::new(match algorithm {
        Algorithm::Sha1 => mac::<Hmac<sha1::Sha1>>(secret, &msg),
        Algorithm::Sha256 => mac::<Hmac<sha2::Sha256>>(secret, &msg),
        Algorithm::Sha512 => mac::<Hmac<sha2::Sha512>>(secret, &msg),
    });
    // Dynamic truncation: the low nibble of the last byte picks 4 bytes.
    let offset = digest.last().map_or(0, |b| (b & 0x0f) as usize);
    let word = digest
        .get(offset..offset + 4)
        .map_or(0, |b| u32::from_be_bytes([b[0] & 0x7f, b[1], b[2], b[3]]));
    let digits = digits.clamp(MIN_DIGITS, MAX_DIGITS);
    word % 10u32.pow(digits)
}

fn mac<M: Mac + KeyInit>(key: &[u8], msg: &[u8]) -> Vec<u8> {
    // HMAC takes keys of any length, so this cannot fail.
    match <M as KeyInit>::new_from_slice(key) {
        Ok(mut m) => {
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// The TOTP time step for `unix_time` (RFC 6238 section 4, T0 = 0).
pub fn totp_counter(unix_time: u64, period: u32) -> u64 {
    unix_time / u64::from(period.max(MIN_PERIOD))
}

/// TOTP value at `unix_time`.
pub fn totp(secret: &[u8], unix_time: u64, period: u32, algorithm: Algorithm, digits: u32) -> u32 {
    hotp(secret, totp_counter(unix_time, period), algorithm, digits)
}

/// Seconds left before the code for `unix_time` changes.
pub fn seconds_left(unix_time: u64, period: u32) -> u32 {
    let period = u64::from(period.max(MIN_PERIOD));
    (period - unix_time % period) as u32
}

/// `code` zero-padded to `digits`.
pub fn format_code(code: u32, digits: u32) -> String {
    format!("{code:0width$}", width = digits as usize)
}

/// A code split in two halves for reading (`123 456`, `1234 5678`).
pub fn group_code(code: &str) -> String {
    let mid = code.len().div_ceil(2);
    match (code.get(..mid), code.get(mid..)) {
        (Some(a), Some(b)) if !b.is_empty() => format!("{a} {b}"),
        _ => code.to_string(),
    }
}

/// The current Unix time in seconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Encodes `bytes` as unpadded upper-case base32 (the `otpauth` convention).
pub fn base32_encode(bytes: &[u8]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(bytes.len().div_ceil(5) * 8));
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        buffer = (buffer << 8) | u64::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decodes a base32 secret as people paste it: any case, with spaces,
/// dashes or `=` padding. Returns `None` for any other character, or for an
/// empty secret.
pub fn base32_decode(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::with_capacity(text.len() * 5 / 8 + 1));
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    let mut padding = false;
    for c in text.chars() {
        let v = match c {
            'A'..='Z' => c as u64 - 'A' as u64,
            'a'..='z' => c as u64 - 'a' as u64,
            '2'..='7' => c as u64 - '2' as u64 + 26,
            ' ' | '\t' | '\n' | '\r' | '-' => continue,
            '=' => {
                padding = true;
                continue;
            }
            _ => return None,
        };
        if padding {
            // Data after padding is not base32.
            return None;
        }
        buffer = ((buffer << 5) | v) & 0xffff;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC4226_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc4226_hotp_vectors() {
        // RFC 4226 appendix D.
        let expected = [
            755224, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (counter, want) in expected.into_iter().enumerate() {
            assert_eq!(hotp(RFC4226_SECRET, counter as u64, Algorithm::Sha1, 6), want);
        }
    }

    #[test]
    fn rfc6238_totp_vectors() {
        // RFC 6238 appendix B: 8 digits, 30 s, seeds of 20/32/64 bytes.
        let sha1 = b"12345678901234567890".as_slice();
        let sha256 = b"12345678901234567890123456789012".as_slice();
        let sha512 = b"1234567890123456789012345678901234567890123456789012345678901234".as_slice();
        let table: [(u64, u32, u32, u32); 6] = [
            (59, 94287082, 46119246, 90693936),
            (1111111109, 7081804, 68084774, 25091201),
            (1111111111, 14050471, 67062674, 99943326),
            (1234567890, 89005924, 91819424, 93441116),
            (2000000000, 69279037, 90698825, 38618901),
            (20000000000, 65353130, 77737706, 47863826),
        ];
        for (t, a, b, c) in table {
            assert_eq!(totp(sha1, t, 30, Algorithm::Sha1, 8), a, "sha1 at {t}");
            assert_eq!(totp(sha256, t, 30, Algorithm::Sha256, 8), b, "sha256 at {t}");
            assert_eq!(totp(sha512, t, 30, Algorithm::Sha512, 8), c, "sha512 at {t}");
        }
        assert_eq!(format_code(7081804, 8), "07081804");
    }

    #[test]
    fn periods_and_countdown() {
        assert_eq!(totp_counter(59, 30), 1);
        assert_eq!(totp_counter(59, 60), 0);
        assert_eq!(seconds_left(59, 30), 1);
        assert_eq!(seconds_left(60, 30), 30);
        // A zero period never divides by zero.
        assert_eq!(totp_counter(5, 0), 5);
    }

    #[test]
    fn code_grouping() {
        assert_eq!(group_code("123456"), "123 456");
        assert_eq!(group_code("1234567"), "1234 567");
        assert_eq!(group_code("12345678"), "1234 5678");
    }

    #[test]
    fn base32_round_trip_and_tolerance() {
        // RFC 4648 section 10 vectors.
        for (raw, enc) in [
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32_encode(raw.as_bytes()).as_str(), enc);
            assert_eq!(base32_decode(enc).unwrap().as_slice(), raw.as_bytes());
        }
        assert_eq!(base32_decode("MZXW6YQ=").unwrap().as_slice(), b"foob");
        let spaced = base32_decode("jbsw y3dp ehpk-3pxp").unwrap();
        assert_eq!(spaced.as_slice(), b"Hello!\xde\xad\xbe\xef");
        assert!(base32_decode("JBSW1").is_none(), "1 is not base32");
        assert!(base32_decode("").is_none());
        assert!(base32_decode("MY==MY").is_none());
    }

    #[test]
    fn algorithm_names() {
        for a in Algorithm::ALL {
            assert_eq!(Algorithm::parse(a.name()), Some(a));
        }
        assert_eq!(Algorithm::parse("sha-256"), Some(Algorithm::Sha256));
        assert_eq!(Algorithm::parse("MD5"), None);
    }
}
