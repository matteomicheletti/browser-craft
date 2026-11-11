//! Firefox (and LibreWolf, Waterfox): bookmarks and history from a copy of
//! `places.sqlite`.
//!
//! - `moz_bookmarks`: type 1 bookmark (its `fk` is a `moz_places` row), 2
//!   folder, 3 separator; the roots have fixed GUIDs. Times
//!   (`dateAdded`) are microseconds since the Unix epoch.
//! - `moz_historyvisits`: one row per visit (`place_id`, `visit_date` in
//!   microseconds since the Unix epoch, `visit_type`).
//!
//! The toolbar goes to the bookmarks bar; the menu, "Other Bookmarks" and
//! mobile roots become folders in "Other bookmarks". Tags, separators and
//! `place:` queries are skipped.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use super::Summary;
use crate::bookmarks::{ImportItem, ImportedBookmarks, MAX_DEPTH};
use crate::history::{ImportPlace, TRANSITION_LINK, TRANSITION_TYPED};

const TOOLBAR: &str = "toolbar_____";
const MENU: &str = "menu________";
const UNFILED: &str = "unfiled_____";
const MOBILE: &str = "mobile______";

/// Firefox's `visit_type`s that are not pages the user saw: embedded
/// content and framed links.
const SKIPPED_VISIT_TYPES: [i64; 2] = [4, 8];
const TYPE_TYPED: i64 = 2;

fn err(e: rusqlite::Error) -> String {
    format!("places.sqlite: {e}")
}

struct Row {
    kind: i64,
    title: String,
    added: i64,
    url: Option<String>,
}

/// The bookmarks of a places.sqlite.
pub fn bookmarks(db: &Connection) -> Result<ImportedBookmarks, String> {
    let mut stmt = db
        .prepare(
            "SELECT b.id, b.type, b.parent, IFNULL(b.title, ''), IFNULL(b.dateAdded, 0), IFNULL(b.guid, ''), p.url
             FROM moz_bookmarks b LEFT JOIN moz_places p ON p.id = b.fk
             ORDER BY b.parent, b.position",
        )
        .map_err(err)?;
    let mut rows: HashMap<i64, Row> = HashMap::new();
    let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut roots: HashMap<String, i64> = HashMap::new();
    let mut query = stmt.query([]).map_err(err)?;
    while let Some(r) = query.next().map_err(err)? {
        let id: i64 = r.get(0).map_err(err)?;
        let parent: i64 = r.get(2).map_err(err)?;
        let guid: String = r.get(5).map_err(err)?;
        if [TOOLBAR, MENU, UNFILED, MOBILE].contains(&guid.as_str()) {
            roots.insert(guid, id);
        }
        rows.insert(
            id,
            Row {
                kind: r.get(1).map_err(err)?,
                title: r.get(3).map_err(err)?,
                added: r.get::<_, i64>(4).map_err(err)? / 1_000_000,
                url: r.get(6).map_err(err)?,
            },
        );
        children.entry(parent).or_default().push(id);
    }
    let mut seen = HashSet::new();
    let mut items = |guid: &str| roots.get(guid).map(|id| build(*id, &rows, &children, &mut seen, 0)).unwrap_or_default();
    let bar = items(TOOLBAR);
    let mut other = Vec::new();
    for (guid, title) in [(MENU, "Bookmarks Menu"), (UNFILED, "Other Bookmarks"), (MOBILE, "Mobile")] {
        let list = items(guid);
        if !list.is_empty() {
            other.push(ImportItem::Folder { title: title.into(), added: 0, items: list });
        }
    }
    Ok(ImportedBookmarks { bar_title: "Bookmarks Toolbar".into(), bar, other })
}

/// The items in folder `id`. `seen` guards against cycles in a damaged
/// database; nesting is cut at `MAX_DEPTH`.
fn build(id: i64, rows: &HashMap<i64, Row>, children: &HashMap<i64, Vec<i64>>, seen: &mut HashSet<i64>, depth: usize) -> Vec<ImportItem> {
    if depth > MAX_DEPTH || !seen.insert(id) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for child in children.get(&id).into_iter().flatten() {
        let Some(row) = rows.get(child) else { continue };
        match row.kind {
            1 => {
                let Some(url) = &row.url else { continue };
                if url.starts_with("place:") {
                    continue;
                }
                out.push(ImportItem::Link { title: row.title.clone(), url: url.clone(), added: row.added });
            }
            2 => out.push(ImportItem::Folder {
                title: row.title.clone(),
                added: row.added,
                items: build(*child, rows, children, seen, depth + 1),
            }),
            _ => {}
        }
    }
    out
}

/// History visited since `since` (microseconds since the Unix epoch), at
/// most `max_visits` visits (the newest kept).
pub fn history(db: &Connection, since: i64, max_visits: usize) -> Result<Vec<ImportPlace>, String> {
    let mut stmt = db
        .prepare(
            "SELECT v.place_id, p.url, IFNULL(p.title, ''), v.visit_date, v.visit_type
             FROM moz_historyvisits v JOIN moz_places p ON p.id = v.place_id
             WHERE v.visit_date >= ?1 AND IFNULL(p.hidden, 0) = 0
             ORDER BY v.visit_date DESC LIMIT ?2",
        )
        .map_err(err)?;
    let mut places: HashMap<i64, ImportPlace> = HashMap::new();
    let mut rows = stmt.query(rusqlite::params![since, max_visits as i64]).map_err(err)?;
    while let Some(r) = rows.next().map_err(err)? {
        let kind: i64 = r.get::<_, Option<i64>>(4).map_err(err)?.unwrap_or(1);
        if SKIPPED_VISIT_TYPES.contains(&kind) {
            continue;
        }
        let Some(time) = r.get::<_, Option<i64>>(3).map_err(err)? else { continue };
        let id: i64 = r.get(0).map_err(err)?;
        let place = match places.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let url: String = r.get(1).map_err(err)?;
                let title: String = r.get(2).map_err(err)?;
                e.insert(ImportPlace { url, title, visits: Vec::new() })
            }
        };
        place.visits.push((time, if kind == TYPE_TYPED { TRANSITION_TYPED } else { TRANSITION_LINK }));
    }
    Ok(places.into_values().collect())
}

/// Counts for the profile list.
pub fn summary(db: &Connection) -> Result<Summary, String> {
    let one = |sql: &str| db.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).map_err(err);
    Ok(Summary {
        bookmarks: one(
            "SELECT COUNT(*) FROM moz_bookmarks b JOIN moz_places p ON p.id = b.fk
             WHERE b.type = 1 AND p.url NOT LIKE 'place:%'
             AND b.parent NOT IN (SELECT id FROM moz_bookmarks WHERE parent = (SELECT id FROM moz_bookmarks WHERE guid = 'tags________'))",
        )?
        .unwrap_or(0) as usize,
        pages: one("SELECT COUNT(*) FROM moz_places WHERE last_visit_date IS NOT NULL AND IFNULL(hidden, 0) = 0")?.unwrap_or(0) as usize,
        visits: one("SELECT COUNT(*) FROM moz_historyvisits")?.unwrap_or(0) as usize,
        last_visit: one("SELECT MAX(visit_date) FROM moz_historyvisits")?,
    })
}

/// The tables and rows this importer reads, as Firefox creates them (the
/// columns used here), for tests.
#[cfg(test)]
pub fn fixture(path: &std::path::Path) -> Connection {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR, rev_host LONGVARCHAR,
            visit_count INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0 NOT NULL, typed INTEGER DEFAULT 0 NOT NULL,
            frecency INTEGER DEFAULT -1 NOT NULL, last_visit_date INTEGER, guid TEXT, url_hash INTEGER DEFAULT 0 NOT NULL);
         CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, from_visit INTEGER, place_id INTEGER,
            visit_date INTEGER, visit_type INTEGER, session INTEGER, source INTEGER DEFAULT 0 NOT NULL, triggeringPlaceId INTEGER);
         CREATE TABLE moz_bookmarks (id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER DEFAULT NULL, parent INTEGER,
            position INTEGER, title LONGVARCHAR, keyword_id INTEGER, folder_type TEXT, dateAdded INTEGER,
            lastModified INTEGER, guid TEXT, syncStatus INTEGER NOT NULL DEFAULT 0, syncChangeCounter INTEGER NOT NULL DEFAULT 1);
         INSERT INTO moz_bookmarks(id, type, parent, position, title, guid) VALUES
            (1, 2, 0, 0, '', 'root________'),
            (2, 2, 1, 0, 'menu', 'menu________'),
            (3, 2, 1, 1, 'toolbar', 'toolbar_____'),
            (4, 2, 1, 2, 'tags', 'tags________'),
            (5, 2, 1, 3, 'unfiled', 'unfiled_____'),
            (6, 2, 1, 4, 'mobile', 'mobile______');",
    )
    .unwrap();
    db
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_700_000_000_000_000;

    fn sample() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture(&dir.path().join("places.sqlite"));
        db.execute_batch(&format!(
            "INSERT INTO moz_places(id, url, title, hidden, last_visit_date) VALUES
                (10, 'https://rust-lang.org/', 'Rust', 0, {T}),
                (11, 'https://news.test/', 'News', 0, {t2}),
                (12, 'place:sort=8&maxResults=10', 'Recent', 0, NULL),
                (13, 'https://tagged.test/', 'Tagged', 0, NULL),
                (14, 'https://hidden.test/redirect', NULL, 1, {T}),
                (15, 'https://menu.test/', 'Menu item', 0, NULL);
             INSERT INTO moz_bookmarks(id, type, fk, parent, position, title, dateAdded, guid) VALUES
                (20, 1, 10, 3, 0, 'Rust', {T}, 'a'),
                (21, 2, NULL, 3, 1, 'Dev', {T}, 'b'),
                (22, 1, 11, 21, 0, 'News', {T}, 'c'),
                (23, 3, NULL, 3, 2, NULL, {T}, 'd'),
                (24, 1, 12, 3, 3, 'Most visited', {T}, 'e'),
                (25, 2, NULL, 4, 0, 'mytag', {T}, 'f'),
                (26, 1, 13, 25, 0, NULL, {T}, 'g'),
                (27, 1, 15, 2, 0, 'Menu item', {T}, 'h'),
                (28, 1, 11, 6, 0, 'News on phone', {T}, 'i');
             INSERT INTO moz_historyvisits(place_id, visit_date, visit_type) VALUES
                (10, {T}, 1), (10, {t1}, 2), (11, {t2}, 1), (11, {t3}, 4), (14, {T}, 5), (10, {old}, 1);",
            t1 = T + 1000,
            t2 = T + 2000,
            t3 = T + 3000,
            old = T - 400 * 86_400_000_000,
        ))
        .unwrap();
        (dir, db)
    }

    #[test]
    fn maps_roots_and_skips_tags_separators_queries() {
        let (_dir, db) = sample();
        let b = bookmarks(&db).unwrap();
        assert_eq!(b.bar_title, "Bookmarks Toolbar");
        assert_eq!(
            b.bar,
            vec![
                ImportItem::Link { title: "Rust".into(), url: "https://rust-lang.org/".into(), added: T / 1_000_000 },
                ImportItem::Folder {
                    title: "Dev".into(),
                    added: T / 1_000_000,
                    items: vec![ImportItem::Link { title: "News".into(), url: "https://news.test/".into(), added: T / 1_000_000 }],
                },
            ]
        );
        let other: Vec<&str> = b
            .other
            .iter()
            .map(|i| match i {
                ImportItem::Folder { title, .. } => title.as_str(),
                ImportItem::Link { .. } => "link",
            })
            .collect();
        assert_eq!(other, ["Bookmarks Menu", "Mobile"], "empty roots are left out; tags never come");
        assert_eq!(b.counts(), (4, 3));
        assert_eq!(summary(&db).unwrap().bookmarks, 4, "tags and place: queries are not counted");
    }

    #[test]
    fn history_with_range_and_visit_types() {
        let (_dir, db) = sample();
        let mut h = history(&db, 0, 1000).unwrap();
        h.sort_by(|a, b| a.url.cmp(&b.url));
        assert_eq!(h.len(), 2, "hidden places are skipped");
        assert_eq!(h[0].url, "https://news.test/");
        assert_eq!(h[0].visits, vec![(T + 2000, TRANSITION_LINK)], "embedded visit skipped");
        let rust = &h[1];
        assert_eq!(rust.visits.len(), 3);
        assert!(rust.visits.contains(&(T + 1000, TRANSITION_TYPED)));
        let recent = history(&db, T - 86_400_000_000, 1000).unwrap();
        assert_eq!(recent.iter().map(|p| p.visits.len()).sum::<usize>(), 3, "the old visit is outside the range");
        let capped = history(&db, 0, 2).unwrap();
        assert_eq!(capped.iter().map(|p| p.visits.len()).sum::<usize>(), 1, "newest first under the cap (an embed is dropped)");
        let s = summary(&db).unwrap();
        assert_eq!((s.visits, s.last_visit), (6, Some(T + 3000)));
    }

    #[test]
    fn cycles_and_depth_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture(&dir.path().join("places.sqlite"));
        // Folder 30 is its own grandparent; 40.. is a chain deeper than MAX_DEPTH.
        db.execute_batch("INSERT INTO moz_bookmarks(id, type, parent, position, title) VALUES (30, 2, 3, 0, 'a'), (31, 2, 30, 0, 'b');
                          UPDATE moz_bookmarks SET parent = 31 WHERE id = 30;")
            .unwrap();
        let mut parent = 5;
        for i in 0..100 {
            db.execute("INSERT INTO moz_bookmarks(id, type, parent, position, title) VALUES (?1, 2, ?2, 0, 'deep')", [100 + i, parent]).unwrap();
            parent = 100 + i;
        }
        let b = bookmarks(&db).unwrap();
        assert!(b.bar.is_empty());
        let (_, folders) = b.counts();
        assert!(folders <= MAX_DEPTH + 2, "{folders}");
    }

    #[test]
    fn not_a_places_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join("x.sqlite")).unwrap();
        assert!(bookmarks(&db).is_err());
        assert!(history(&db, 0, 10).is_err());
    }
}
