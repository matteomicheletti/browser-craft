//! Chrome, Chromium, Brave, Edge, Vivaldi, Opera: bookmarks from the
//! profile's `Bookmarks` JSON file, history from a copy of its `History`
//! database (tables `urls` and `visits`).
//!
//! Chromium's times are microseconds since 1601-01-01 UTC (the Windows
//! epoch): `CHROME_EPOCH_OFFSET_US` converts them. Bookmarks keep them as
//! decimal strings.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use serde_json::Value;

use super::Summary;
use crate::bookmarks::{ImportItem, ImportedBookmarks, MAX_DEPTH};
use crate::history::{ImportPlace, TRANSITION_LINK, TRANSITION_TYPED};

/// Microseconds from 1601-01-01 to 1970-01-01.
pub const CHROME_EPOCH_OFFSET_US: i64 = 11_644_473_600_000_000;
/// The largest Bookmarks file read.
pub const MAX_BOOKMARKS_FILE: u64 = 64 << 20;

/// Core transition types (the low byte of `visits.transition`).
const CORE_TYPED: i64 = 1;
const CORE_AUTO_SUBFRAME: i64 = 3;
const CORE_MANUAL_SUBFRAME: i64 = 4;

/// Chromium time (microseconds since 1601) to microseconds since 1970;
/// `None` for 0 (unknown).
pub fn to_unix_us(chrome: i64) -> Option<i64> {
    (chrome > 0).then(|| chrome - CHROME_EPOCH_OFFSET_US)
}

/// Microseconds since 1970 to Chromium time.
pub fn from_unix_us(unix: i64) -> i64 {
    unix + CHROME_EPOCH_OFFSET_US
}

fn err(e: rusqlite::Error) -> String {
    format!("History: {e}")
}

/// Reads and parses the profile's Bookmarks file.
pub fn bookmarks_file(path: &Path) -> Result<ImportedBookmarks, String> {
    let text = crate::storage::read_text_limited(path, MAX_BOOKMARKS_FILE).map_err(|e| format!("{}: {e}", path.display()))?;
    bookmarks(&text)
}

/// Parses a Bookmarks file: `roots.bookmark_bar` goes to the bar,
/// `roots.other` to "Other bookmarks", `roots.synced` to its "Mobile"
/// folder.
pub fn bookmarks(text: &str) -> Result<ImportedBookmarks, String> {
    // serde_json refuses nesting beyond 128 levels.
    let json: Value = serde_json::from_str(text).map_err(|e| format!("Bookmarks is not valid JSON: {e}"))?;
    let roots = json.get("roots").filter(|r| r.is_object()).ok_or("Bookmarks has no roots")?;
    let items = |name: &str| roots.get(name).map(|n| children(n, 0)).unwrap_or_default();
    let mut other = items("other");
    let synced = items("synced");
    if !synced.is_empty() {
        other.push(ImportItem::Folder { title: "Mobile".into(), added: 0, items: synced });
    }
    Ok(ImportedBookmarks { bar_title: "Bookmarks bar".into(), bar: items("bookmark_bar"), other })
}

fn added_secs(node: &Value) -> i64 {
    node["date_added"].as_str().and_then(|s| s.parse::<i64>().ok()).and_then(to_unix_us).map_or(0, |us| us / 1_000_000)
}

fn children(folder: &Value, depth: usize) -> Vec<ImportItem> {
    if depth > MAX_DEPTH {
        return Vec::new();
    }
    let Some(list) = folder["children"].as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for node in list {
        let title = node["name"].as_str().unwrap_or("").to_string();
        match node["type"].as_str() {
            Some("url") => {
                if let Some(url) = node["url"].as_str() {
                    out.push(ImportItem::Link { title, url: url.to_string(), added: added_secs(node) });
                }
            }
            Some("folder") => {
                out.push(ImportItem::Folder { title, added: added_secs(node), items: children(node, depth + 1) })
            }
            _ => {}
        }
    }
    out
}

/// History visited since `since` (microseconds since the Unix epoch), at
/// most `max_visits` visits (the newest kept). Sub-frame navigations are
/// not pages the user saw and are skipped.
pub fn history(db: &Connection, since: i64, max_visits: usize) -> Result<Vec<ImportPlace>, String> {
    let sql = |hidden: bool| {
        format!(
            "SELECT u.id, u.url, IFNULL(u.title, ''), v.visit_time, v.transition
             FROM visits v JOIN urls u ON u.id = v.url
             WHERE v.visit_time >= ?1 {}
             ORDER BY v.visit_time DESC LIMIT ?2",
            if hidden { "AND u.hidden = 0" } else { "" }
        )
    };
    // Older schemas have no `hidden` column.
    let mut stmt = match db.prepare(&sql(true)) {
        Ok(stmt) => stmt,
        Err(_) => db.prepare(&sql(false)).map_err(err)?,
    };
    let mut places: HashMap<i64, ImportPlace> = HashMap::new();
    let mut rows = stmt.query(rusqlite::params![from_unix_us(since.max(0)), max_visits as i64]).map_err(err)?;
    while let Some(r) = rows.next().map_err(err)? {
        let core = r.get::<_, Option<i64>>(4).map_err(err)?.unwrap_or(0) & 0xff;
        if core == CORE_AUTO_SUBFRAME || core == CORE_MANUAL_SUBFRAME {
            continue;
        }
        let Some(time) = r.get::<_, Option<i64>>(3).map_err(err)?.and_then(to_unix_us) else { continue };
        let id: i64 = r.get(0).map_err(err)?;
        let place = match places.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let url: String = r.get(1).map_err(err)?;
                let title: String = r.get(2).map_err(err)?;
                e.insert(ImportPlace { url, title, visits: Vec::new() })
            }
        };
        place.visits.push((time, if core == CORE_TYPED { TRANSITION_TYPED } else { TRANSITION_LINK }));
    }
    Ok(places.into_values().collect())
}

/// History counts for the profile list (bookmarks are counted from the
/// JSON file).
pub fn history_summary(db: &Connection) -> Result<Summary, String> {
    let one = |sql: &str| db.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).map_err(err);
    Ok(Summary {
        bookmarks: 0,
        pages: one("SELECT COUNT(*) FROM urls")?.unwrap_or(0) as usize,
        visits: one("SELECT COUNT(*) FROM visits")?.unwrap_or(0) as usize,
        last_visit: one("SELECT MAX(visit_time) FROM visits")?.and_then(to_unix_us),
    })
}

/// The tables this importer reads, as Chromium creates them (the columns
/// used here), for tests.
#[cfg(test)]
pub fn fixture(path: &Path) -> Connection {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE urls(id INTEGER PRIMARY KEY AUTOINCREMENT, url LONGVARCHAR, title LONGVARCHAR,
            visit_count INTEGER DEFAULT 0 NOT NULL, typed_count INTEGER DEFAULT 0 NOT NULL,
            last_visit_time INTEGER NOT NULL, hidden INTEGER DEFAULT 0 NOT NULL);
         CREATE TABLE visits(id INTEGER PRIMARY KEY AUTOINCREMENT, url INTEGER NOT NULL, visit_time INTEGER NOT NULL,
            from_visit INTEGER, transition INTEGER DEFAULT 0 NOT NULL, segment_id INTEGER, visit_duration INTEGER DEFAULT 0 NOT NULL);",
    )
    .unwrap();
    db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_conversion() {
        // 2024-01-01T00:00:00Z: 1704067200 s since 1970, 13348540800 s since 1601.
        assert_eq!(to_unix_us(13_348_540_800_000_000), Some(1_704_067_200_000_000));
        assert_eq!(from_unix_us(1_704_067_200_000_000), 13_348_540_800_000_000);
        assert_eq!(to_unix_us(0), None);
        assert_eq!(to_unix_us(CHROME_EPOCH_OFFSET_US), Some(0), "1970-01-01");
    }

    const SAMPLE: &str = r#"{
        "checksum": "x", "version": 1,
        "roots": {
            "bookmark_bar": {"type": "folder", "name": "Bookmarks bar", "children": [
                {"type": "url", "name": "Rust", "url": "https://rust-lang.org/", "date_added": "13348540800000000"},
                {"type": "folder", "name": "Work", "date_added": "0", "children": [
                    {"type": "url", "name": "Mail", "url": "https://mail.test/"},
                    {"type": "url", "name": "Script", "url": "javascript:alert(1)"}
                ]}
            ]},
            "other": {"type": "folder", "name": "Other bookmarks", "children": [
                {"type": "url", "name": "Other one", "url": "https://other.test/"},
                {"type": "separator"},
                {"type": "url", "name": "No url"}
            ]},
            "synced": {"type": "folder", "name": "Mobile bookmarks", "children": [
                {"type": "url", "name": "Phone", "url": "https://phone.test/"}
            ]}
        }
    }"#;

    #[test]
    fn maps_roots_and_times() {
        let b = bookmarks(SAMPLE).unwrap();
        assert_eq!(b.bar_title, "Bookmarks bar");
        assert_eq!(b.bar[0], ImportItem::Link { title: "Rust".into(), url: "https://rust-lang.org/".into(), added: 1_704_067_200 });
        assert!(matches!(&b.bar[1], ImportItem::Folder { title, items, added: 0 } if title == "Work" && items.len() == 2));
        assert_eq!(b.other[0], ImportItem::Link { title: "Other one".into(), url: "https://other.test/".into(), added: 0 });
        assert!(matches!(&b.other[1], ImportItem::Folder { title, items, .. } if title == "Mobile" && items.len() == 1));
        assert_eq!(b.counts(), (5, 2), "separators and links without an address are skipped");
    }

    #[test]
    fn malformed_bookmarks() {
        assert!(bookmarks("").is_err());
        assert!(bookmarks("{\"roots\": 3}").is_err());
        assert_eq!(bookmarks("{\"roots\": {}}").unwrap().counts(), (0, 0));
        let b = bookmarks(r#"{"roots":{"bookmark_bar":{"children":[{"type":"url","url":7},{"type":"folder","children":"x"}]}}}"#).unwrap();
        assert_eq!(b.counts(), (0, 1));
        // Deeper than serde_json's limit: an error, not a crash.
        let deep = format!("{{\"roots\":{{\"other\":{}}}}}", "{\"type\":\"folder\",\"children\":[".repeat(300));
        assert!(bookmarks(&deep).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(bookmarks_file(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn history_with_range_transitions_and_subframes() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture(&dir.path().join("History"));
        let t = 13_348_540_800_000_000_i64; // 2024-01-01
        let unix = 1_704_067_200_000_000_i64;
        db.execute_batch(&format!(
            "INSERT INTO urls(id, url, title, last_visit_time, hidden) VALUES
                (1, 'https://a.test/', 'A', {t}, 0), (2, 'https://frame.test/', 'F', {t}, 0), (3, 'https://hid.test/', '', {t}, 1);
             INSERT INTO visits(url, visit_time, transition) VALUES
                (1, {t}, 805306369), (1, {t1}, 0), (2, {t}, 3), (3, {t}, 0), (1, {old}, 0), (1, 0, 0);",
            t1 = t + 1_000_000,
            old = t - 365 * 86_400_000_000,
        ))
        .unwrap();
        let h = history(&db, 0, 100).unwrap();
        assert_eq!(h.len(), 1, "sub-frames and hidden addresses are skipped");
        let mut visits = h[0].visits.clone();
        visits.sort();
        assert_eq!(visits[0], (unix - 365 * 86_400_000_000, TRANSITION_LINK));
        assert!(visits.contains(&(unix, TRANSITION_TYPED)), "typed, with qualifier bits");
        assert_eq!(visits.len(), 3, "a zero time is skipped");
        let recent = history(&db, unix - 86_400_000_000, 100).unwrap();
        assert_eq!(recent[0].visits.len(), 2);
        let s = history_summary(&db).unwrap();
        assert_eq!((s.pages, s.visits, s.last_visit), (3, 6, Some(unix + 1_000_000)));
    }
}
