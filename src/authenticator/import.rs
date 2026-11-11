//! Getting accounts in and out of the authenticator:
//!
//! * BrowseCraft encrypted backups (see [`super::vault`]), both ways;
//! * plain lists of `otpauth://` URIs, one per line, both ways (export only
//!   after the user confirmed they want an unencrypted file);
//! * `otpauth-migration://` lines (Google Authenticator exports), in;
//! * Aegis JSON exports, plain or password-encrypted, in (format: Aegis
//!   `docs/vault.md`; TOTP and HOTP entries, other types are skipped);
//! * QR codes in PNG, JPEG, GIF or WebP images, in.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use serde_json::Value;
use zeroize::Zeroizing;

use super::otp::Algorithm;
use super::vault::{self, BACKUP_MAGIC, VAULT_MAGIC};
use super::{Account, Kind, Secret, uri};

/// Largest file read for an import.
pub const MAX_IMPORT: u64 = 16 << 20;
/// Largest image side decoded when looking for a QR code.
pub const MAX_IMAGE_SIDE: u32 = 8000;

/// What an import found.
#[derive(Debug, Default)]
pub struct Imported {
    pub accounts: Vec<Account>,
    /// Entries that could not be used (unsupported type, bad secret…).
    pub skipped: usize,
}

/// The kind of file an import was given, from its first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Backup,
    /// The vault file itself: open it with its password like a backup.
    Vault,
    AegisPlain,
    AegisEncrypted,
    Image,
    UriList,
    Unknown,
}

impl FileKind {
    pub fn needs_password(self) -> bool {
        matches!(self, FileKind::Backup | FileKind::Vault | FileKind::AegisEncrypted)
    }

    pub fn describe(self) -> &'static str {
        match self {
            FileKind::Backup => "BrowseCraft encrypted backup",
            FileKind::Vault => "BrowseCraft authenticator vault",
            FileKind::AegisPlain => "Aegis export (not encrypted)",
            FileKind::AegisEncrypted => "Aegis export (encrypted)",
            FileKind::Image => "image (QR code)",
            FileKind::UriList => "list of otpauth:// links",
            FileKind::Unknown => "unrecognised file",
        }
    }
}

pub fn detect(bytes: &[u8]) -> FileKind {
    if vault::has_magic(bytes, BACKUP_MAGIC) {
        return FileKind::Backup;
    }
    if vault::has_magic(bytes, VAULT_MAGIC) {
        return FileKind::Vault;
    }
    if bytes.starts_with(b"\x89PNG")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"GIF8")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
    {
        return FileKind::Image;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return FileKind::Unknown;
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with('{') {
        return match serde_json::from_str::<Value>(text) {
            Ok(v) if v.get("header").is_some() && v.get("db").is_some() => {
                if v["db"].is_string() {
                    FileKind::AegisEncrypted
                } else {
                    FileKind::AegisPlain
                }
            }
            _ => FileKind::Unknown,
        };
    }
    if text.lines().any(|l| l.trim_start().to_ascii_lowercase().starts_with("otpauth")) {
        return FileKind::UriList;
    }
    FileKind::Unknown
}

/// Reads an import file (size-limited).
pub fn read_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    crate::storage::read_limited(path, MAX_IMPORT)
        .map(Zeroizing::new)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))
}

/// Imports from file contents of any supported kind. `password` is used for
/// encrypted kinds. May be slow (key derivation): run it off the UI thread.
pub fn import_bytes(bytes: &[u8], password: Option<&str>) -> Result<Imported, String> {
    let need = || password.ok_or_else(|| "This file is encrypted: enter its password.".to_string());
    match detect(bytes) {
        FileKind::Backup => Ok(Imported {
            accounts: vault::restore_backup(bytes, need()?)?,
            skipped: 0,
        }),
        FileKind::Vault => {
            let (_, plain) = vault::unseal(bytes, VAULT_MAGIC, need()?)?;
            Ok(Imported {
                accounts: vault::Contents::from_json(&plain)?.accounts,
                skipped: 0,
            })
        }
        FileKind::AegisPlain | FileKind::AegisEncrypted => parse_aegis(bytes, password),
        FileKind::Image => {
            let mut out = Imported::default();
            for text in decode_qr(bytes)? {
                let found = parse_uri_list(&text);
                out.accounts.extend(found.accounts);
                out.skipped += found.skipped;
            }
            if out.accounts.is_empty() && out.skipped > 0 {
                return Err("The QR code does not hold an authenticator link.".into());
            }
            Ok(out)
        }
        FileKind::UriList => {
            let text = std::str::from_utf8(bytes).map_err(|_| "The file is not text.")?;
            Ok(parse_uri_list(text))
        }
        FileKind::Unknown => Err("This file is not a format the authenticator can import.".into()),
    }
}

/// Parses `otpauth://` and `otpauth-migration://` links, one per line;
/// blank lines and `#` comments are ignored, anything else is skipped.
pub fn parse_uri_list(text: &str) -> Imported {
    let mut out = Imported::default();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lower = line.get(..20).unwrap_or(line).to_ascii_lowercase();
        if lower.starts_with("otpauth-migration:") {
            match uri::parse_migration(line) {
                Ok(m) => {
                    out.accounts.extend(m.accounts);
                    out.skipped += m.skipped;
                }
                Err(_) => out.skipped += 1,
            }
        } else {
            match uri::parse(line) {
                Ok(a) => out.accounts.push(a),
                Err(_) => out.skipped += 1,
            }
        }
    }
    out
}

/// One `otpauth://` link per line. Holds every secret in clear.
pub fn uri_list(accounts: &[Account]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(accounts.len() * 160));
    for a in accounts {
        out.push_str(&uri::to_uri(a));
        out.push('\n');
    }
    out
}

/// Writes an export to a new file (owner-only); never replaces a file.
pub fn write_export(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::storage::create_new(path, bytes, true).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => "A file with that name already exists; pick a new name.".to_string(),
        _ => format!("Could not write {}: {e}", path.display()),
    })
}

// ---- QR images ----

/// Decodes every QR code found in an image file.
pub fn decode_qr(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Could not read the image: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| format!("Could not decode the image: {e}"))?
        .to_luma8();
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| image.get_pixel(x as u32, y as u32).0[0]);
    let found: Vec<String> = prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .collect();
    if found.is_empty() {
        return Err("No QR code was found in the image.".into());
    }
    Ok(found)
}

// ---- Aegis ----

fn hex(text: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return Err("Invalid hex in the Aegis file.".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| text.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect::<Option<Vec<u8>>>()
        .map(Zeroizing::new)
        .ok_or_else(|| "Invalid hex in the Aegis file.".into())
}

fn aes_gcm_open(key: &[u8], nonce_hex: &str, tag_hex: &str, data: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let nonce = hex(nonce_hex).ok()?;
    let tag = hex(tag_hex).ok()?;
    let nonce = Nonce::try_from(nonce.as_slice()).ok()?;
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let mut msg = Zeroizing::new(Vec::with_capacity(data.len() + tag.len()));
    msg.extend_from_slice(data);
    msg.extend_from_slice(&tag);
    cipher
        .decrypt(&nonce, Payload { msg: &msg, aad: &[] })
        .ok()
        .map(Zeroizing::new)
}

/// Unwraps the master key from the password slots (type 1): the password
/// through scrypt (the slot's n, r, p, salt) opens the slot's AES-256-GCM
/// wrapped key.
fn aegis_master_key(header: &Value, password: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let slots = header["slots"].as_array().ok_or("The Aegis file has no key slots.")?;
    let mut any_password_slot = false;
    for slot in slots.iter().filter(|s| s["type"].as_u64() == Some(1)) {
        any_password_slot = true;
        let n = slot["n"].as_u64().unwrap_or(0);
        let r = slot["r"].as_u64().unwrap_or(0);
        let p = slot["p"].as_u64().unwrap_or(0);
        // Bounded so a crafted file cannot ask for gigabytes (128·r·N bytes).
        if !n.is_power_of_two() || n < 2 || !(1..=32).contains(&r) || !(1..=16).contains(&p) || 128 * r * n > 512 << 20
        {
            return Err("The Aegis file asks for scrypt settings outside the allowed range.".into());
        }
        let params = scrypt::Params::new(n.trailing_zeros() as u8, r as u32, p as u32)
            .map_err(|_| "Invalid scrypt settings in the Aegis file.")?;
        let salt = hex(slot["salt"].as_str().unwrap_or(""))?;
        let mut derived = Zeroizing::new([0u8; 32]);
        scrypt::scrypt(password.as_bytes(), &salt, &params, derived.as_mut_slice())
            .map_err(|_| "Key derivation failed.")?;
        let wrapped = hex(slot["key"].as_str().unwrap_or(""))?;
        let kp = &slot["key_params"];
        if let Some(key) = aes_gcm_open(
            derived.as_slice(),
            kp["nonce"].as_str().unwrap_or(""),
            kp["tag"].as_str().unwrap_or(""),
            &wrapped,
        ) {
            return Ok(key);
        }
    }
    Err(if any_password_slot {
        "Wrong password for the Aegis file.".into()
    } else {
        "The Aegis file has no password slot (biometric-only exports cannot be read).".into()
    })
}

/// Parses an Aegis export, decrypting it first when it is encrypted.
pub fn parse_aegis(bytes: &[u8], password: Option<&str>) -> Result<Imported, String> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| "The Aegis file is not valid JSON.")?;
    if root["version"].as_u64() != Some(1) {
        return Err("Unsupported Aegis file version.".into());
    }
    let db: Value = match &root["db"] {
        Value::String(sealed) => {
            let password = password.ok_or("This Aegis file is encrypted: enter its password.")?;
            let key = aegis_master_key(&root["header"], password)?;
            let data = Zeroizing::new(
                base64::engine::general_purpose::STANDARD
                    .decode(sealed.as_bytes())
                    .map_err(|_| "The Aegis database is not valid base64.")?,
            );
            let params = &root["header"]["params"];
            let plain = aes_gcm_open(
                &key,
                params["nonce"].as_str().unwrap_or(""),
                params["tag"].as_str().unwrap_or(""),
                &data,
            )
            .ok_or("The Aegis database could not be decrypted (damaged file).")?;
            serde_json::from_slice(&plain).map_err(|_| "The decrypted Aegis database is not valid JSON.")?
        }
        Value::Object(_) => root["db"].clone(),
        _ => return Err("The Aegis file has no database.".into()),
    };
    let version = db["version"].as_u64().unwrap_or(0);
    if !(1..=3).contains(&version) {
        return Err(format!("Unsupported Aegis database version {version}."));
    }
    let groups: Vec<(String, String)> = db["groups"]
        .as_array()
        .map(|g| {
            g.iter()
                .filter_map(|g| Some((g["uuid"].as_str()?.to_string(), g["name"].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Imported::default();
    for entry in db["entries"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        match aegis_entry(entry, &groups) {
            Some(a) => out.accounts.push(a),
            None => out.skipped += 1,
        }
    }
    Ok(out)
}

fn aegis_entry(entry: &Value, groups: &[(String, String)]) -> Option<Account> {
    let info = &entry["info"];
    let kind = match entry["type"].as_str()? {
        "totp" => Kind::Totp {
            period: u32::try_from(info["period"].as_u64().unwrap_or(30)).ok()?,
        },
        "hotp" => Kind::Hotp {
            counter: info["counter"].as_u64()?,
        },
        _ => return None, // steam, yandex, motp: other algorithms
    };
    let mut tags: Vec<String> = Vec::new();
    // Database version 3: group uuids; version 2: one group name.
    if let Some(ids) = entry["groups"].as_array() {
        for id in ids.iter().filter_map(Value::as_str) {
            if let Some((_, name)) = groups.iter().find(|(uuid, _)| uuid == id) {
                tags.push(name.clone());
            }
        }
    }
    if let Some(name) = entry["group"].as_str() {
        tags.push(name.to_string());
    }
    let mut account = Account {
        id: super::new_id(),
        issuer: entry["issuer"].as_str().unwrap_or("").to_string(),
        label: entry["name"].as_str().unwrap_or("").to_string(),
        secret: Secret::from_base32(info["secret"].as_str()?)?,
        algorithm: Algorithm::parse(info["algo"].as_str().unwrap_or("SHA1"))?,
        digits: u32::try_from(info["digits"].as_u64().unwrap_or(6)).ok()?,
        kind,
        tags,
        note: entry["note"].as_str().unwrap_or("").to_string(),
    };
    account.tidy();
    account.validate().ok()?;
    Some(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn accounts() -> Vec<Account> {
        let mut a = Account::totp("GitHub", "alice", Secret::from_base32("JBSWY3DPEHPK3PXP").unwrap());
        a.tags = vec!["work".into()];
        let mut b = Account::totp("", "bank", Secret::from_slice(b"12345678901234567890"));
        b.kind = Kind::Hotp { counter: 3 };
        b.algorithm = Algorithm::Sha256;
        b.digits = 8;
        vec![a, b]
    }

    #[test]
    fn uri_list_round_trip_and_detection() {
        let list = uri_list(&accounts());
        assert_eq!(detect(list.as_bytes()), FileKind::UriList);
        let text = format!("# exported\n\n{}not a link\n", list.as_str());
        let back = parse_uri_list(&text);
        assert_eq!(back.skipped, 1);
        assert_eq!(back.accounts.len(), 2);
        for (a, b) in accounts().iter().zip(&back.accounts) {
            assert!(a.same_as(b));
            assert_eq!((a.algorithm, a.digits, a.kind), (b.algorithm, b.digits, b.kind));
        }
        let imported = import_bytes(list.as_bytes(), None).unwrap();
        assert_eq!(imported.accounts.len(), 2);
    }

    #[test]
    fn backup_import_through_detection() {
        let original = accounts();
        let bytes = vault::backup_bytes(&original, "backup password").unwrap();
        assert_eq!(detect(&bytes), FileKind::Backup);
        assert!(detect(&bytes).needs_password());
        assert!(import_bytes(&bytes, None).is_err());
        assert!(import_bytes(&bytes, Some("nope nope nope")).is_err());
        let got = import_bytes(&bytes, Some("backup password")).unwrap();
        assert_eq!(got.accounts, original);
    }

    #[test]
    fn export_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codes.txt");
        write_export(&path, b"one").unwrap();
        assert!(write_export(&path, b"two").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
    }

    fn aegis_db() -> Value {
        // Shaped after Aegis docs/vault.md (database version 3).
        json!({
            "version": 3,
            "entries": [
                {
                    "type": "totp", "uuid": "01234567-89ab-cdef-0123-456789abcdef",
                    "name": "alice@example.com", "issuer": "Deno", "note": "main",
                    "favorite": false, "icon": null,
                    "info": {"secret": "4SJHB4GSD43FZBAI7C2HLRJGPQ", "algo": "SHA1", "digits": 6, "period": 30},
                    "groups": ["11111111-1111-1111-1111-111111111111"]
                },
                {
                    "type": "hotp", "uuid": "2", "name": "bob", "issuer": "SPDX", "note": "",
                    "info": {"secret": "5OM4WOOGPLQEF6UGN3CPEOOLWU", "algo": "SHA256", "digits": 7, "counter": 10},
                    "groups": []
                },
                {
                    "type": "steam", "uuid": "3", "name": "gaben", "issuer": "Steam",
                    "info": {"secret": "JRZCL47CMXVOQMNPZR2F7J4RGI", "algo": "SHA1", "digits": 5, "period": 30}
                }
            ],
            "groups": [{"uuid": "11111111-1111-1111-1111-111111111111", "name": "Personal"}]
        })
    }

    fn check_aegis(got: &Imported) {
        assert_eq!(got.skipped, 1, "steam entry");
        let a = &got.accounts[0];
        assert_eq!((a.issuer.as_str(), a.label.as_str(), a.note.as_str()), ("Deno", "alice@example.com", "main"));
        assert_eq!(a.tags, vec!["Personal".to_string()]);
        assert_eq!(a.secret.to_base32().as_str(), "4SJHB4GSD43FZBAI7C2HLRJGPQ");
        let b = &got.accounts[1];
        assert_eq!((b.algorithm, b.digits, b.kind), (Algorithm::Sha256, 7, Kind::Hotp { counter: 10 }));
    }

    #[test]
    fn aegis_plain() {
        let file = json!({"version": 1, "header": {"slots": null, "params": null}, "db": aegis_db()});
        let bytes = serde_json::to_vec(&file).unwrap();
        assert_eq!(detect(&bytes), FileKind::AegisPlain);
        check_aegis(&import_bytes(&bytes, None).unwrap());
        // Database version 2 used a single group name.
        let mut db = aegis_db();
        db["version"] = json!(2);
        db["entries"][0].as_object_mut().unwrap().remove("groups");
        db["entries"][0]["group"] = json!("Old group");
        let file = json!({"version": 1, "header": {"slots": null, "params": null}, "db": db});
        let got = parse_aegis(&serde_json::to_vec(&file).unwrap(), None).unwrap();
        assert_eq!(got.accounts[0].tags, vec!["Old group".to_string()]);
    }

    fn to_hex(b: &[u8]) -> String {
        b.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Seals with AES-256-GCM and returns (ciphertext, nonce hex, tag hex).
    fn gcm_seal(key: &[u8], nonce: [u8; 12], plain: &[u8]) -> (Vec<u8>, String, String) {
        let cipher = Aes256Gcm::new_from_slice(key).unwrap();
        let mut out = cipher.encrypt(&Nonce::from(nonce), plain).unwrap();
        let tag = out.split_off(out.len() - 16);
        (out, to_hex(&nonce), to_hex(&tag))
    }

    /// Builds an encrypted export the way Aegis does: a random master key
    /// wrapped in a password slot (scrypt), the database sealed with it.
    fn aegis_encrypted(password: &str) -> Vec<u8> {
        let master = [7u8; 32];
        let salt = [9u8; 32];
        let (n, r, p) = (1024u64, 8u64, 1u64);
        let mut derived = [0u8; 32];
        scrypt::scrypt(
            password.as_bytes(),
            &salt,
            &scrypt::Params::new(10, r as u32, p as u32).unwrap(),
            &mut derived,
        )
        .unwrap();
        let (wrapped, key_nonce, key_tag) = gcm_seal(&derived, [1; 12], &master);
        let (db, db_nonce, db_tag) = gcm_seal(&master, [2; 12], &serde_json::to_vec(&aegis_db()).unwrap());
        let file = json!({
            "version": 1,
            "header": {
                "slots": [
                    {"type": 2, "uuid": "bio", "key": "00", "key_params": {"nonce": "00", "tag": "00"}},
                    {
                        "type": 1, "uuid": "a8325752-c1be-458a-9b3e-5e0a8154d9ec",
                        "key": to_hex(&wrapped), "key_params": {"nonce": key_nonce, "tag": key_tag},
                        "n": n, "r": r, "p": p, "salt": to_hex(&salt), "repaired": true, "is_backup": false
                    }
                ],
                "params": {"nonce": db_nonce, "tag": db_tag}
            },
            "db": base64::engine::general_purpose::STANDARD.encode(db)
        });
        serde_json::to_vec(&file).unwrap()
    }

    #[test]
    fn aegis_encrypted_round() {
        let bytes = aegis_encrypted("aegis pass");
        assert_eq!(detect(&bytes), FileKind::AegisEncrypted);
        assert!(import_bytes(&bytes, None).is_err());
        assert!(import_bytes(&bytes, Some("wrong")).unwrap_err().contains("Wrong password"));
        check_aegis(&import_bytes(&bytes, Some("aegis pass")).unwrap());
        // A slot demanding 1 TiB of scrypt memory is refused.
        let mut v: Value = serde_json::from_slice(&bytes).unwrap();
        v["header"]["slots"][1]["n"] = json!(1u64 << 30);
        let err = parse_aegis(&serde_json::to_vec(&v).unwrap(), Some("aegis pass")).unwrap_err();
        assert!(err.contains("range"), "{err}");
    }

    /// Renders `text` as a QR code PNG, 6 px per module with a quiet zone.
    fn qr_png(text: &str) -> Vec<u8> {
        let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let (w, colors) = (code.width(), code.to_colors());
        let scale = 6;
        let side = ((w + 8) * scale) as u32;
        let img = image::GrayImage::from_fn(side, side, |x, y| {
            let (mx, my) = ((x as usize / scale) as isize - 4, (y as usize / scale) as isize - 4);
            let dark = mx >= 0
                && my >= 0
                && (mx as usize) < w
                && (my as usize) < w
                && colors[my as usize * w + mx as usize] == qrcode::Color::Dark;
            image::Luma([if dark { 0 } else { 255 }])
        });
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        png
    }

    #[test]
    fn qr_image_import() {
        let link = "otpauth://totp/Example:alice@google.com?secret=JBSWY3DPEHPK3PXP&issuer=Example";
        let png = qr_png(link);
        assert_eq!(detect(&png), FileKind::Image);
        assert_eq!(decode_qr(&png).unwrap(), vec![link.to_string()]);
        let got = import_bytes(&png, None).unwrap();
        assert_eq!(got.accounts.len(), 1);
        assert_eq!(got.accounts[0].issuer, "Example");

        // A Google Authenticator transfer QR code.
        let migration = "otpauth-migration://offline?data=CjEKCkhlbGxvId6tvu8SGEV4YW1wbGU6YWxpY2VAZ29vZ2xlLmNvbRoHRXhhbXBsZSABKAEwAhABGAEgACjr4JKrBA%3D%3D";
        let got = import_bytes(&qr_png(migration), None).unwrap();
        assert_eq!(got.accounts[0].label, "alice@google.com");

        // A QR code with something else in it, and no QR code at all.
        assert!(import_bytes(&qr_png("https://example.com"), None).is_err());
        let blank = image::GrayImage::from_pixel(64, 64, image::Luma([255]));
        let mut png = Vec::new();
        blank.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert!(decode_qr(&png).unwrap_err().contains("No QR code"));
        assert!(decode_qr(b"\x89PNG garbage").is_err());
    }

    #[test]
    fn unknown_files() {
        assert_eq!(detect(b"hello"), FileKind::Unknown);
        assert_eq!(detect(b"{\"a\":1}"), FileKind::Unknown);
        assert_eq!(detect(&[0xff, 0xfe, 0x00]), FileKind::Unknown);
        assert!(import_bytes(b"hello", None).is_err());
    }
}
