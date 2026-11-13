//! `browsecraft passwords import-csv FILE`: brings the logins exported by
//! Firefox (about:logins → ⋯ → Export Passwords) or Chrome into the vault.
//!
//! Firefox and Chrome write their passwords in clear text in that CSV: the
//! command reads it, adds the logins to the KeePass vault (made, with a new
//! password, when there is none yet) and offers to delete the file.
//! Logins already in the vault (same site and user name) are skipped.

use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::vault::{NewProtection, Unlock, Vault};

const USAGE: &str = "usage:
  browsecraft passwords import-csv FILE [--keep-file]
FILE: the CSV exported by Firefox (about:logins, menu, Export Passwords) or Chrome.
Without a vault yet, one is made in the configuration folder with a new password.
The CSV holds the passwords in clear text: it is deleted afterwards unless --keep-file.";

/// Largest CSV accepted.
const MAX_CSV: u64 = 64 << 20;

pub fn main(args: &[String]) -> i32 {
    let (file, keep) = match args {
        [cmd, file] if cmd == "import-csv" => (PathBuf::from(file), false),
        [cmd, file, flag] if cmd == "import-csv" && flag == "--keep-file" => (PathBuf::from(file), true),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match import_csv(&file, keep) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(e) => {
            eprintln!("browsecraft: {e}");
            1
        }
    }
}

fn import_csv(file: &Path, keep: bool) -> Result<String, String> {
    let bytes = crate::storage::read_limited(file, MAX_CSV).map_err(|e| format!("Could not read {}: {e}", file.display()))?;
    let text = Zeroizing::new(String::from_utf8_lossy(&bytes).into_owned());
    let logins = parse_logins(&text)?;
    if logins.is_empty() {
        return Err("No logins found in the file.".into());
    }
    crate::passwords::load();
    let (configured, mode, keyfile) = crate::passwords::configured_vault();
    let mut vault = match configured.filter(|p| p.is_file()) {
        Some(path) => {
            let unlock = match (mode, keyfile) {
                (crate::passwords::Mode::KeyFile, Some(k)) => Unlock::KeyFile(k),
                _ => Unlock::Password(crate::prompt::password(&format!("Password of the vault {}: ", path.display()))?),
            };
            Vault::open(&path, &unlock)?
        }
        None => {
            let path = crate::passwords::default_vault();
            eprintln!("No password vault yet: a new one is made at {}.", path.display());
            eprintln!("Choose its password (it unlocks your passwords in the browser and in KeePassXC).");
            let password = crate::prompt::new_password("New vault password: ", 8)?;
            let vault = Vault::create(&path, NewProtection::Password(password))?;
            crate::passwords::remember_vault(&path);
            vault
        }
    };
    let existing: std::collections::HashSet<(String, String)> =
        vault.credentials().into_iter().map(|c| (crate::vault::host_key(&c.url), c.username)).collect();
    let total = logins.len();
    let mut seen = existing;
    let new: Vec<_> = logins
        .into_iter()
        .filter(|(_, url, user, _)| seen.insert((crate::vault::host_key(url), user.clone())))
        .collect();
    let added = vault.add_many(&new)?;
    let mut line = format!(
        "Imported {added} logins into {} ({} already there or repeated in the file).",
        vault.path().display(),
        total - added
    );
    if !keep {
        match std::fs::remove_file(file) {
            Ok(()) => line += &format!("\nDeleted {} (it held the passwords in clear text).", file.display()),
            Err(e) => line += &format!("\nCould not delete {}: {e}. Delete it yourself.", file.display()),
        }
    }
    Ok(line)
}

/// Title, URL, user name and password of each row. Firefox's header is
/// `url,username,password,httpRealm,...`, Chrome's `name,url,username,password,...`.
pub fn parse_logins(text: &str) -> Result<Vec<(String, String, String, Zeroizing<String>)>, String> {
    let mut rows = parse_csv(text).into_iter();
    let header: Vec<String> = rows.next().ok_or("The file is empty.")?.into_iter().map(|h| h.trim().to_lowercase()).collect();
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(url), Some(user), Some(pass)) = (col("url").or(col("origin")), col("username"), col("password")) else {
        return Err("This is not a password export: the first line needs url, username and password columns.".into());
    };
    let title = col("name").or(col("title"));
    let mut out = Vec::new();
    for row in rows {
        let get = |i: usize| row.get(i).cloned().unwrap_or_default();
        let (u, p) = (get(url), Zeroizing::new(get(pass)));
        if p.is_empty() || u.is_empty() {
            continue;
        }
        let t = title.map(get).unwrap_or_default();
        out.push((t, u, get(user), p));
    }
    Ok(out)
}

/// RFC 4180 CSV: quoted fields, doubled quotes, line breaks inside quotes.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firefox_and_chrome_exports() {
        let firefox = "\"url\",\"username\",\"password\",\"httpRealm\",\"formActionOrigin\",\"guid\"\r\n\
            \"https://a.example\",\"me\",\"p,\"\"1\",,\"\",\"{x}\"\r\n\
            \"https://b.example\",\"\",\"two\nlines\",,,\"{y}\"\r\n\
            \"https://c.example\",\"x\",\"\",,,\"{z}\"\r\n";
        let l = parse_logins(firefox).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].1.as_str(), l[0].2.as_str(), l[0].3.as_str()), ("https://a.example", "me", "p,\"1"));
        assert_eq!(l[1].3.as_str(), "two\nlines");
        let chrome = "name,url,username,password,note\nSite,https://d.example/login,u,pw,\n";
        let l = parse_logins(chrome).unwrap();
        assert_eq!((l[0].0.as_str(), l[0].1.as_str()), ("Site", "https://d.example/login"));
        assert!(parse_logins("a,b\n1,2\n").is_err());
    }
}
