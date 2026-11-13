//! Reads the passwords Firefox saved (`logins.json`), decrypting them with
//! the key in the profile's NSS database (`key4.db`). No clear-text file is
//! written on the way.
//!
//! The layout (Firefox 75 and later):
//!
//! * `key4.db`, table `metaData`, row `password`: `item1` is the global
//!   salt, `item2` the encrypted text `password-check`, which tells whether
//!   the primary password (empty when the user set none) is right.
//! * table `nssPrivate`: `a11` is the encrypted key, `a102` its id.
//! * Both are PKCS#5 PBES2: PBKDF2-HMAC-SHA256 over SHA-1(global salt +
//!   primary password), then AES-256-CBC.
//! * `logins.json`: user names and passwords as base64 DER
//!   `SEQUENCE { key id, SEQUENCE { aes256-CBC, iv }, ciphertext }`.
//!
//! Older profiles with 3DES keys are refused with a hint to export a CSV.

use std::path::Path;

use aes::Aes256;
use aes::cipher::{BlockCipherDecrypt, KeyInit};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha1::Digest as _;
use zeroize::Zeroizing;

const OID_PBES2: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0d];
const OID_AES256_CBC: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x2a];
/// The id of the key that encrypts the logins.
const LOGIN_KEY_ID: &[u8] = &[0xf8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
/// PBKDF2 rounds accepted (Firefox uses 10 000).
const MAX_ROUNDS: u64 = 10_000_000;

/// Why the passwords could not be read.
#[derive(Debug, PartialEq)]
pub enum Error {
    /// The profile has a primary password and this one is wrong (or empty).
    PrimaryPassword,
    Other(String),
}

impl From<String> for Error {
    fn from(e: String) -> Self {
        Error::Other(e)
    }
}

/// A login: title (the site), URL, user name, password.
pub type Login = (String, String, String, Zeroizing<String>);

/// The logins of the Firefox profile in `dir`, decrypted with `primary`
/// (empty when the user set no primary password).
pub fn read(dir: &Path, primary: &str) -> Result<Vec<Login>, Error> {
    let logins = dir.join("logins.json");
    if !logins.is_file() {
        return Ok(Vec::new());
    }
    let key = login_key(dir, primary)?;
    let text = crate::storage::read_text_limited(&logins, 64 << 20).map_err(|e| format!("Could not read logins.json: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("logins.json: {e}"))?;
    let mut out = Vec::new();
    for login in json["logins"].as_array().into_iter().flatten() {
        let field = |name: &str| login[name].as_str().unwrap_or("");
        let (Some(user), Some(pass)) = (decrypt_field(&key, field("encryptedUsername")), decrypt_field(&key, field("encryptedPassword"))) else {
            continue;
        };
        if pass.is_empty() {
            continue;
        }
        let url = field("hostname").to_string();
        out.push((String::new(), url, user.to_string(), pass));
    }
    Ok(out)
}

/// The key that decrypts the logins, from `key4.db` (read from a copy).
fn login_key(dir: &Path, primary: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
    let db = super::open_copy(&dir.join("key4.db"))?;
    let conn = &db.conn;
    let (salt, check): (Vec<u8>, Vec<u8>) = conn
        .query_row("SELECT item1, item2 FROM metaData WHERE id = 'password'", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| format!("key4.db: {e}"))?;
    let checked = pbes2_decrypt(&check, &salt, primary).map_err(|_| Error::PrimaryPassword)?;
    if !checked.starts_with(b"password-check") {
        return Err(Error::PrimaryPassword);
    }
    let mut stmt = conn.prepare("SELECT a11, a102 FROM nssPrivate").map_err(|e| format!("key4.db: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)))
        .map_err(|e| format!("key4.db: {e}"))?;
    for (a11, id) in rows.flatten() {
        if id == LOGIN_KEY_ID {
            let key = pbes2_decrypt(&a11, &salt, primary)?;
            if key.len() < 32 {
                return Err(Error::Other(
                    "This Firefox profile uses an old key type: export the passwords as CSV instead.".into(),
                ));
            }
            return Ok(Zeroizing::new(key[..32].to_vec()));
        }
    }
    Err(Error::Other("key4.db holds no key for the saved passwords.".into()))
}

/// A base64 DER login field.
fn decrypt_field(key: &[u8], b64: &str) -> Option<Zeroizing<String>> {
    let der = BASE64.decode(b64).ok()?;
    let mut top = Der::new(&der).seq()?;
    let _key_id = top.octets()?;
    let mut algo = top.seq()?;
    if algo.oid()? != OID_AES256_CBC {
        return None;
    }
    let iv = algo.octets()?;
    let ct = top.octets()?;
    let plain = aes_cbc_decrypt(key, iv, ct)?;
    String::from_utf8(plain.to_vec()).ok().map(Zeroizing::new)
}

/// PKCS#5 PBES2 as NSS writes it: PBKDF2-HMAC-SHA256 over
/// SHA-1(global salt + password), AES-256-CBC with a 14-byte IV that NSS
/// stores without its DER header (`04 0e`).
fn pbes2_decrypt(der: &[u8], global_salt: &[u8], password: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let bad = || "Unsupported key format in key4.db: export the passwords as CSV instead.".to_string();
    let mut top = Der::new(der).seq().ok_or_else(bad)?;
    let mut algo = top.seq().ok_or_else(bad)?;
    if algo.oid().ok_or_else(bad)? != OID_PBES2 {
        return Err(bad());
    }
    let mut params = algo.seq().ok_or_else(bad)?;
    let mut kdf = params.seq().ok_or_else(bad)?;
    let _pbkdf2 = kdf.oid().ok_or_else(bad)?;
    let mut kdf_params = kdf.seq().ok_or_else(bad)?;
    let salt = kdf_params.octets().ok_or_else(bad)?;
    let rounds = kdf_params.integer().ok_or_else(bad)?;
    if rounds == 0 || rounds > MAX_ROUNDS {
        return Err(bad());
    }
    let mut cipher = params.seq().ok_or_else(bad)?;
    if cipher.oid().ok_or_else(bad)? != OID_AES256_CBC {
        return Err(bad());
    }
    let iv_tail = cipher.octets().ok_or_else(bad)?;
    let ct = top.octets().ok_or_else(bad)?;
    let mut hasher = sha1::Sha1::new();
    hasher.update(global_salt);
    hasher.update(password.as_bytes());
    let k = Zeroizing::new(hasher.finalize().to_vec());
    let mut key = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(&k, salt, rounds as u32, key.as_mut_slice());
    let mut iv = vec![0x04, 0x0e];
    iv.extend_from_slice(iv_tail);
    aes_cbc_decrypt(key.as_slice(), &iv, ct).ok_or_else(|| "wrong key".to_string())
}

/// AES-256-CBC with PKCS#7 padding removed; `None` on a bad key (the
/// padding does not check out).
fn aes_cbc_decrypt(key: &[u8], iv: &[u8], ct: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if key.len() != 32 || iv.len() != 16 || ct.is_empty() || ct.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes256::new_from_slice(key).ok()?;
    let mut out = Zeroizing::new(Vec::with_capacity(ct.len()));
    let mut prev: [u8; 16] = iv.try_into().ok()?;
    for chunk in ct.chunks(16) {
        let mut block = aes::Block::try_from(chunk).ok()?;
        cipher.decrypt_block(&mut block);
        for (b, p) in block.iter_mut().zip(prev.iter()) {
            *b ^= p;
        }
        out.extend_from_slice(&block);
        prev = chunk.try_into().ok()?;
    }
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || pad > out.len() || !out[out.len() - pad..].iter().all(|b| *b as usize == pad) {
        return None;
    }
    let len = out.len() - pad;
    out.truncate(len);
    Some(out)
}

/// Just enough DER: definite lengths, the few types NSS writes.
struct Der<'a> {
    rest: &'a [u8],
}

impl<'a> Der<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Der { rest: bytes }
    }

    /// The next element with `tag`: its content.
    fn take(&mut self, tag: u8) -> Option<&'a [u8]> {
        let (&t, rest) = self.rest.split_first()?;
        if t != tag {
            return None;
        }
        let (&first, mut rest) = rest.split_first()?;
        let len = if first & 0x80 == 0 {
            first as usize
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 || rest.len() < n {
                return None;
            }
            let len = rest[..n].iter().fold(0usize, |acc, b| (acc << 8) | *b as usize);
            rest = &rest[n..];
            len
        };
        if rest.len() < len {
            return None;
        }
        let (content, after) = rest.split_at(len);
        self.rest = after;
        Some(content)
    }

    fn seq(&mut self) -> Option<Der<'a>> {
        self.take(0x30).map(Der::new)
    }

    fn octets(&mut self) -> Option<&'a [u8]> {
        self.take(0x04)
    }

    fn oid(&mut self) -> Option<&'a [u8]> {
        self.take(0x06)
    }

    fn integer(&mut self) -> Option<u64> {
        let bytes = self.take(0x02)?;
        if bytes.len() > 8 {
            return None;
        }
        Some(bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_lengths_and_padding() {
        // SEQUENCE { OCTET STRING 01 02, INTEGER 10000 }
        let der = [0x30, 0x08, 0x04, 0x02, 1, 2, 0x02, 0x02, 0x27, 0x10];
        let mut s = Der::new(&der).seq().unwrap();
        assert_eq!(s.octets().unwrap(), &[1, 2]);
        assert_eq!(s.integer().unwrap(), 10_000);
        assert!(Der::new(&[0x30, 0x05, 0x04]).seq().is_none());
        // A wrong key leaves garbage padding: refused.
        assert!(aes_cbc_decrypt(&[7u8; 32], &[0u8; 16], &[0u8; 16]).is_none());
    }
}
