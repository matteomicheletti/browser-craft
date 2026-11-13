//! `browsecraft profile export FILE` and `browsecraft profile import FILE`:
//! moves a BrowseCraft set-up to another computer in one encrypted file.
//!
//! The file holds the settings (flags, containers and their rules, dark
//! mode, session presets, user agents, site permissions), the bookmarks, the
//! history, the password vault (still a KeePass file with its own password)
//! and the authenticator vault (encrypted with its own password too). It is
//! encrypted as a whole with a password chosen at export: Argon2id (64 MiB,
//! 3 passes) and XChaCha20-Poly1305, the authenticator's container
//! (`authenticator::vault::seal`) with its own magic.
//!
//! Left out on purpose: cookies and site data (the web engines differ
//! between macOS and Linux, and logged-in sessions should not travel), open
//! windows, plugin folders (paths of this computer) and automation settings.
//!
//! Import replaces the files it brings and keeps the previous ones in a
//! `backup-<time>` folder next to them. The browser must be closed.

use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::authenticator::vault::{VaultKey, seal, unseal};

const MAGIC: &[u8; 8] = b"BCPROFL1";
/// Largest profile file read back.
const MAX_FILE: u64 = 2 << 30;
const MIN_PASSWORD: usize = 10;
/// Settings files copied as they are, from the configuration folder.
const CONFIG_FILES: &[&str] = &[
    "flags.txt",
    "containers.json",
    "presets.json",
    "bookmarks.json",
    "user-agents.txt",
    "darkmode.json",
    "permissions.json",
    "authenticator.bcotp",
];
const HISTORY: &str = "history.sqlite";
const VAULT: &str = "browsecraft-passwords.kdbx";
const KEYFILE: &str = "browsecraft-passwords.keyx";
const PASSWORDS_CFG: &str = "passwords.cfg";

const USAGE: &str = "usage:
  browsecraft profile export FILE   settings, bookmarks, history and vaults in one encrypted file
  browsecraft profile import FILE   replaces this computer's (close BrowseCraft first; old files are kept in a backup folder)
The password is asked in the terminal (at least 10 characters).";

pub fn main(args: &[String]) -> i32 {
    let result = match args {
        [cmd, file] if cmd == "export" => export(Path::new(file)),
        [cmd, file] if cmd == "import" => import(Path::new(file)),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("browsecraft: {e}");
            1
        }
    }
}

// ---- the archive inside: (name, bytes) pairs ----

fn pack(files: &[(String, Vec<u8>)]) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::new());
    for (name, data) in files {
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

fn unpack(mut bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let bad = || "The profile file is damaged.".to_string();
    let mut files = Vec::new();
    while !bytes.is_empty() {
        let n = u16::from_le_bytes(bytes.get(..2).ok_or_else(bad)?.try_into().map_err(|_| bad())?) as usize;
        let name = std::str::from_utf8(bytes.get(2..2 + n).ok_or_else(bad)?).map_err(|_| bad())?.to_string();
        bytes = &bytes[2 + n..];
        let len = u64::from_le_bytes(bytes.get(..8).ok_or_else(bad)?.try_into().map_err(|_| bad())?) as usize;
        let data = bytes.get(8..8usize.checked_add(len).ok_or_else(bad)?).ok_or_else(bad)?.to_vec();
        bytes = &bytes[8 + len..];
        // Only plain names: nothing may be written outside the folders.
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(bad());
        }
        files.push((name, data));
    }
    Ok(files)
}

// ---- export ----

fn export(out: &Path) -> Result<String, String> {
    if out.exists() {
        return Err(format!("{} already exists: choose another name.", out.display()));
    }
    let config = crate::storage::config_dir()?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for name in CONFIG_FILES {
        if let Ok(bytes) = std::fs::read(config.join(name)) {
            files.push((name.to_string(), bytes));
        }
    }
    // The history through SQLite (consistent even while the browser runs).
    let db = crate::history::db_path()?;
    if db.is_file() {
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let copy = tmp.path().join(HISTORY);
        crate::history::Db::open(&db)?.backup_to(&copy)?;
        files.push((HISTORY.into(), std::fs::read(&copy).map_err(|e| e.to_string())?));
    }
    crate::passwords::load();
    let (vault, mode, keyfile) = crate::passwords::configured_vault();
    if let Some(bytes) = vault.as_ref().and_then(|p| std::fs::read(p).ok()) {
        files.push((VAULT.into(), bytes));
        let mut cfg = String::from(if mode == crate::passwords::Mode::KeyFile { "mode=keyfile\n" } else { "mode=password\n" });
        if let Some(bytes) = keyfile.as_ref().and_then(|p| std::fs::read(p).ok()) {
            files.push((KEYFILE.into(), bytes));
            cfg += "keyfile=1\n";
        }
        files.push((PASSWORDS_CFG.into(), cfg.into_bytes()));
    }
    let names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    eprintln!("Choose the password of the profile file (you type it again when importing).");
    let password = crate::prompt::new_password("Profile password: ", MIN_PASSWORD)?;
    let key = VaultKey::new(&password)?;
    let sealed = seal(MAGIC, &key, &pack(&files))?;
    crate::storage::create_new(out, &sealed, true).map_err(|e| format!("Could not write {}: {e}", out.display()))?;
    Ok(format!("Exported to {} ({} KB): {}", out.display(), sealed.len() / 1024, names.join(", ")))
}

// ---- import ----

/// Another BrowseCraft process of this user is running.
fn browser_running() -> bool {
    #[cfg(unix)]
    {
        let me = std::process::id().to_string();
        if let Ok(out) = std::process::Command::new("pgrep").args(["-x", "browsecraft"]).output() {
            return String::from_utf8_lossy(&out.stdout).lines().any(|pid| pid.trim() != me);
        }
    }
    false
}

fn import(file: &Path) -> Result<String, String> {
    if browser_running() {
        return Err("BrowseCraft is open: close it, then import again.".into());
    }
    let bytes = crate::storage::read_limited(file, MAX_FILE).map_err(|e| format!("Could not read {}: {e}", file.display()))?;
    let password = crate::prompt::password("Profile password: ")?;
    let (_, plain) = unseal(&bytes, MAGIC, &password).map_err(|e| {
        if e.contains("magic") || e.contains("not a") { "This is not a BrowseCraft profile file.".to_string() } else { e }
    })?;
    let files = unpack(&plain)?;
    let config = crate::storage::config_dir()?;
    crate::storage::private_dir(&config).map_err(|e| e.to_string())?;
    let data = crate::history::db_path()?.parent().map(Path::to_path_buf).unwrap_or_else(|| config.clone());
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let backup = config.join(format!("backup-{stamp}"));
    crate::storage::private_dir(&backup).map_err(|e| e.to_string())?;
    let keep_old = |path: &Path| -> Result<(), String> {
        if path.exists() {
            let name = path.file_name().ok_or("bad path")?;
            std::fs::rename(path, backup.join(name)).map_err(|e| format!("Could not back up {}: {e}", path.display()))?;
        }
        Ok(())
    };
    let mut written = Vec::new();
    let mut vault_mode: Option<String> = None;
    for (name, bytes) in &files {
        let target: PathBuf = match name.as_str() {
            HISTORY => data.join(HISTORY),
            VAULT | KEYFILE => config.join(name),
            PASSWORDS_CFG => {
                vault_mode = Some(String::from_utf8_lossy(bytes).into_owned());
                continue;
            }
            n if CONFIG_FILES.contains(&n) => config.join(n),
            _ => continue, // from a newer version: ignored
        };
        if name == HISTORY {
            // The old database's journal must not be replayed on the new one.
            for ext in ["-wal", "-shm"] {
                let side = data.join(format!("{HISTORY}{ext}"));
                keep_old(&side)?;
            }
        }
        keep_old(&target)?;
        crate::storage::write_private(&target, bytes).map_err(|e| format!("Could not write {}: {e}", target.display()))?;
        written.push(name.clone());
    }
    if let Some(cfg) = vault_mode {
        // The vault's path on this computer, and the never-save list kept.
        let cfg_path = config.join(PASSWORDS_CFG);
        let old = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        keep_old(&cfg_path)?;
        let mut text = format!("vault={}\n", config.join(VAULT).display());
        text += if cfg.contains("mode=keyfile") { "mode=keyfile\n" } else { "mode=password\n" };
        if cfg.contains("keyfile=1") {
            text += &format!("keyfile={}\n", config.join(KEYFILE).display());
        }
        for line in old.lines().filter(|l| l.starts_with("never=")) {
            text += line;
            text += "\n";
        }
        crate::storage::write_private(&cfg_path, text.as_bytes()).map_err(|e| e.to_string())?;
        written.push(PASSWORDS_CFG.into());
    }
    Ok(format!("Imported: {}\nPrevious files kept in {}", written.join(", "), backup.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_round_trip_and_bad_names() {
        let files = vec![("a.json".to_string(), b"{}".to_vec()), ("history.sqlite".to_string(), vec![0u8; 1000])];
        assert_eq!(unpack(&pack(&files)).unwrap(), files);
        assert!(unpack(&pack(&[("../x".to_string(), vec![1])])).is_err());
        assert!(unpack(&pack(&files)[..10]).is_err());
    }

    #[test]
    fn sealed_with_the_password_only() {
        let key = VaultKey::new("correct horse battery").unwrap();
        let sealed = seal(MAGIC, &key, &pack(&[("flags.txt".into(), b"x".to_vec())])).unwrap();
        assert!(unseal(&sealed, MAGIC, "wrong password!").is_err());
        let (_, plain) = unseal(&sealed, MAGIC, "correct horse battery").unwrap();
        assert_eq!(unpack(&plain).unwrap()[0].0, "flags.txt");
    }
}
