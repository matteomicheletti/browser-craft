//! Browsing history, kept on disk in `history.sqlite` (the per-user data
//! folder, owner-only, WAL journal) and shown by `browsecraft://history`.
//!
//! Two tables, like Firefox's: `places` (one row per address: title, visit
//! count, last visit) and `visits` (one row per visit: place and time, in
//! microseconds since the Unix epoch). Visits recorded while browsing go
//! through a channel to a writer thread, so the UI thread never waits for
//! the disk; reads (the history page on the loader thread, the palette and
//! automation on the UI thread) use a second connection, which WAL lets run
//! beside the writer. `browsecraft import` opens the same file: SQLite's
//! own locking keeps the processes apart.
//!
//! Tabs in temporary containers are never recorded.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::internal::{self, FormToken};

/// Longer URLs are not recorded: a page could otherwise fill the history
/// with megabyte-long addresses.
pub const MAX_URL_LEN: usize = 8 * 1024;
/// Longer titles are cut.
const MAX_TITLE_CHARS: usize = 400;
/// Two loads of one address this close together count as one visit (a
/// load and the title it reports, the engine and the web view both
/// reporting the same page).
const SAME_VISIT_US: i64 = 30 * 1_000_000;
/// Search terms beyond this many are ignored.
const MAX_TERMS: usize = 8;
/// Visits per page of `browsecraft://history`.
const PAGE_SIZE: usize = 150;
const HOUR_US: i64 = 3600 * 1_000_000;
const DAY_US: i64 = 24 * HOUR_US;

/// `visits.transition`: how the visit started (Firefox's and Chromium's
/// "typed" is the one that matters for ranking).
pub const TRANSITION_LINK: i64 = 0;
pub const TRANSITION_TYPED: i64 = 1;

const FILE_NAME: &str = "history.sqlite";

/// Microseconds since the Unix epoch, now.
pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// The address as history keeps it: without a user name and password, and
/// `None` for addresses that are not kept (internal pages, scripts, data
/// URLs, overlong ones).
pub fn clean_url(url: &str) -> Option<String> {
    let url = url.trim();
    let clean = crate::net::without_userinfo(url).unwrap_or_else(|| url.to_string());
    let lower = clean.get(..16).unwrap_or(&clean).to_ascii_lowercase();
    let skipped = ["browsecraft:", "about:", "view-source:", "javascript:", "data:", "blob:", "place:", "chrome:", "edge:"];
    if clean.is_empty() || clean.len() > MAX_URL_LEN || skipped.iter().any(|s| lower.starts_with(s)) {
        return None;
    }
    Some(clean)
}

fn clean_title(title: &str) -> String {
    let collapsed: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    collapsed.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TITLE_CHARS).collect()
}

/// A history search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    /// Microseconds since the Unix epoch.
    pub last_visit: i64,
}

/// One visit, for the history page.
#[derive(Clone, Debug, PartialEq)]
pub struct VisitRow {
    pub url: String,
    pub title: String,
    pub time: i64,
}

/// An address to import with its visits (microseconds since the Unix
/// epoch, and the transition).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportPlace {
    pub url: String,
    pub title: String,
    pub visits: Vec<(i64, i64)>,
}

/// What an import added.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct ImportStats {
    pub import_id: i64,
    pub places_added: usize,
    pub visits_added: usize,
    /// Visits already in the history (same address and time).
    pub visits_skipped: usize,
    /// Addresses history does not keep (internal pages, overlong, ...).
    pub urls_skipped: usize,
}

/// The last import that added history, for "Undo last import".
#[derive(Clone, Debug, PartialEq)]
pub struct ImportRecord {
    pub id: i64,
    pub source: String,
    pub time: i64,
    pub visits: i64,
}

fn sql_err(e: rusqlite::Error) -> String {
    format!("history database: {e}")
}

/// A connection to a history database.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens (creating it owner-only if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Db, String> {
        if let Some(dir) = path.parent() {
            crate::storage::private_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        if !path.exists() {
            // Created owner-only before SQLite writes a byte; the -wal and
            // -shm files take the database file's permissions.
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        Db::init(Connection::open(path).map_err(sql_err)?)
    }

    #[cfg(test)]
    pub fn open_memory() -> Db {
        #[allow(clippy::unwrap_used)]
        Db::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(conn: Connection) -> Result<Db, String> {
        conn.busy_timeout(Duration::from_secs(10)).map_err(sql_err)?;
        // journal_mode answers with a row.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)).map_err(sql_err)?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA secure_delete=ON;
             CREATE TABLE IF NOT EXISTS places(
                 id INTEGER PRIMARY KEY,
                 url TEXT NOT NULL UNIQUE,
                 title TEXT NOT NULL DEFAULT '',
                 visit_count INTEGER NOT NULL DEFAULT 0,
                 typed_count INTEGER NOT NULL DEFAULT 0,
                 last_visit INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS visits(
                 id INTEGER PRIMARY KEY,
                 place_id INTEGER NOT NULL REFERENCES places(id) ON DELETE CASCADE,
                 time INTEGER NOT NULL,
                 transition INTEGER NOT NULL DEFAULT 0,
                 import_id INTEGER NOT NULL DEFAULT 0,
                 UNIQUE(place_id, time));
             CREATE INDEX IF NOT EXISTS visits_time ON visits(time);
             CREATE INDEX IF NOT EXISTS visits_import ON visits(import_id) WHERE import_id <> 0;
             CREATE INDEX IF NOT EXISTS places_last ON places(last_visit);
             CREATE TABLE IF NOT EXISTS imports(
                 id INTEGER PRIMARY KEY,
                 time INTEGER NOT NULL,
                 source TEXT NOT NULL,
                 visits INTEGER NOT NULL DEFAULT 0);
             PRAGMA user_version=1;",
        )
        .map_err(sql_err)?;
        Ok(Db { conn })
    }

    /// Records a visit. Returns whether it was new (not the same address at
    /// the same time). An empty `title` keeps the known one.
    pub fn add_visit(&self, url: &str, title: &str, time: i64, transition: i64) -> Result<bool, String> {
        let title = clean_title(title);
        let place: i64 = self
            .conn
            .query_row(
                "INSERT INTO places(url, title) VALUES(?1, ?2)
                 ON CONFLICT(url) DO UPDATE SET title = CASE WHEN excluded.title <> '' THEN excluded.title ELSE places.title END
                 RETURNING id",
                params![url, title],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        let added = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO visits(place_id, time, transition) VALUES(?1, ?2, ?3)",
                params![place, time, transition],
            )
            .map_err(sql_err)?
            > 0;
        if added {
            self.conn
                .execute(
                    "UPDATE places SET visit_count = visit_count + 1, last_visit = MAX(last_visit, ?2),
                     typed_count = typed_count + ?3 WHERE id = ?1",
                    params![place, time, i64::from(transition == TRANSITION_TYPED)],
                )
                .map_err(sql_err)?;
        }
        Ok(added)
    }

    /// Updates the title of a known address.
    pub fn retitle(&self, url: &str, title: &str) -> Result<(), String> {
        let title = clean_title(title);
        if !title.is_empty() {
            self.conn.execute("UPDATE places SET title = ?2 WHERE url = ?1", params![url, title]).map_err(sql_err)?;
        }
        Ok(())
    }

    /// Up to `limit` addresses whose title or URL contains every word of
    /// `query` (any case), best first: often and recently visited pages
    /// rank higher (Firefox's "frecency", simplified), and a site or title
    /// that starts with the query higher still. An empty query lists the
    /// most recently visited.
    pub fn search(&self, query: &str, limit: usize, now: i64) -> Result<Vec<Hit>, String> {
        let terms: Vec<String> = query.split_whitespace().take(MAX_TERMS).map(like_pattern).collect();
        let limit = limit.clamp(1, 10_000);
        if terms.is_empty() {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT url, title, visit_count, last_visit FROM places ORDER BY last_visit DESC LIMIT ?1")
                .map_err(sql_err)?;
            let rows = stmt.query_map([limit as i64], hit_row).map_err(sql_err)?;
            return rows.collect::<Result<Vec<_>, _>>().map_err(sql_err);
        }
        let filter = term_filter(terms.len(), "url", "title", 2);
        let sql = format!(
            "SELECT url, title, visit_count, last_visit,
                (MIN(visit_count + 2 * typed_count, 100) + 1) *
                (CASE WHEN ?1 - last_visit < {d4} THEN 100 WHEN ?1 - last_visit < {d14} THEN 70
                      WHEN ?1 - last_visit < {d31} THEN 50 WHEN ?1 - last_visit < {d90} THEN 30 ELSE 10 END) AS score
             FROM places WHERE {filter} ORDER BY score DESC, last_visit DESC LIMIT {n}",
            d4 = 4 * DAY_US,
            d14 = 14 * DAY_US,
            d31 = 31 * DAY_US,
            d90 = 90 * DAY_US,
            n = limit * 4,
        );
        let mut values: Vec<rusqlite::types::Value> = vec![now.into()];
        values.extend(terms.into_iter().map(rusqlite::types::Value::from));
        let mut stmt = self.conn.prepare(&sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(params_from_iter(values), |r| Ok((hit_row(r)?, r.get::<_, i64>(4)?)))
            .map_err(sql_err)?;
        let mut hits = rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)?;
        let q = query.trim().to_lowercase();
        let first = q.split_whitespace().next().unwrap_or("").to_string();
        for (hit, score) in hits.iter_mut() {
            let site = site_of(&hit.url);
            if site.starts_with(&first) || hit.title.to_lowercase().starts_with(&q) {
                *score *= 3;
            }
        }
        hits.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.last_visit.cmp(&a.0.last_visit)));
        Ok(hits.into_iter().take(limit).map(|(h, _)| h).collect())
    }

    /// Visits newest first, filtered like `search`, from `offset`. The flag
    /// says whether there are more.
    pub fn visits(&self, query: &str, offset: usize, limit: usize) -> Result<(Vec<VisitRow>, bool), String> {
        let terms: Vec<String> = query.split_whitespace().take(MAX_TERMS).map(like_pattern).collect();
        let filter = if terms.is_empty() { "1".to_string() } else { term_filter(terms.len(), "p.url", "p.title", 1) };
        let sql = format!(
            "SELECT p.url, p.title, v.time FROM visits v JOIN places p ON p.id = v.place_id
             WHERE {filter} ORDER BY v.time DESC LIMIT {} OFFSET {}",
            limit + 1,
            offset
        );
        let mut stmt = self.conn.prepare(&sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(params_from_iter(terms), |r| Ok(VisitRow { url: r.get(0)?, title: r.get(1)?, time: r.get(2)? }))
            .map_err(sql_err)?;
        let mut list = rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)?;
        let more = list.len() > limit;
        list.truncate(limit);
        Ok((list, more))
    }

    /// (addresses, visits).
    pub fn counts(&self) -> Result<(i64, i64), String> {
        let places = self.conn.query_row("SELECT COUNT(*) FROM places", [], |r| r.get(0)).map_err(sql_err)?;
        let visits = self.conn.query_row("SELECT COUNT(*) FROM visits", [], |r| r.get(0)).map_err(sql_err)?;
        Ok((places, visits))
    }

    /// Forgets every visit.
    pub fn clear_all(&self) -> Result<(), String> {
        self.conn.execute_batch("DELETE FROM visits; DELETE FROM places; DELETE FROM imports;").map_err(sql_err)?;
        // Deleted pages are zeroed (secure_delete); also empty the journal.
        let _ = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(())
    }

    /// Forgets the visits from `from` (inclusive) to `to` (exclusive).
    /// Returns how many.
    pub fn clear_range(&self, from: i64, to: i64) -> Result<usize, String> {
        let n = self.conn.execute("DELETE FROM visits WHERE time >= ?1 AND time < ?2", params![from, to]).map_err(sql_err)?;
        if n > 0 {
            self.fix_places()?;
        }
        Ok(n)
    }

    /// Forgets an address and all its visits.
    pub fn remove_url(&self, url: &str) -> Result<bool, String> {
        Ok(self.conn.execute("DELETE FROM places WHERE url = ?1", [url]).map_err(sql_err)? > 0)
    }

    /// Drops visits older than `days` (0: keep all) and the oldest beyond
    /// `max_visits` (0: no cap). Returns how many were dropped.
    pub fn enforce_retention(&self, days: u32, max_visits: u32, now: i64) -> Result<usize, String> {
        let mut n = 0;
        if days > 0 {
            n += self
                .conn
                .execute("DELETE FROM visits WHERE time < ?1", [now - i64::from(days) * DAY_US])
                .map_err(sql_err)?;
        }
        if max_visits > 0 {
            n += self
                .conn
                .execute(
                    "DELETE FROM visits WHERE id IN (SELECT id FROM visits ORDER BY time DESC LIMIT -1 OFFSET ?1)",
                    [i64::from(max_visits)],
                )
                .map_err(sql_err)?;
        }
        if n > 0 {
            self.fix_places()?;
        }
        Ok(n)
    }

    /// Recomputes the per-address counts from the visits and drops
    /// addresses that have none left.
    fn fix_places(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "DELETE FROM places WHERE NOT EXISTS (SELECT 1 FROM visits WHERE place_id = places.id);
                 UPDATE places SET
                    visit_count = (SELECT COUNT(*) FROM visits WHERE place_id = places.id),
                    typed_count = (SELECT COUNT(*) FROM visits WHERE place_id = places.id AND transition = 1),
                    last_visit = (SELECT MAX(time) FROM visits WHERE place_id = places.id);",
            )
            .map_err(sql_err)
    }

    /// Adds imported history in one transaction, tagging the new visits
    /// with a new import id (for `undo_import`). Visits already present
    /// (same address and time) are skipped, and visits before `since`
    /// (microseconds, 0: all) too. `progress` gets the number of addresses
    /// done so far now and then.
    pub fn import(
        &mut self,
        places: &[ImportPlace],
        source: &str,
        since: i64,
        progress: &mut dyn FnMut(usize),
    ) -> Result<ImportStats, String> {
        let tx = self.conn.transaction().map_err(sql_err)?;
        tx.execute("INSERT INTO imports(time, source) VALUES(?1, ?2)", params![now_us(), source]).map_err(sql_err)?;
        let import_id = tx.last_insert_rowid();
        let mut stats = ImportStats { import_id, ..Default::default() };
        {
            let mut find = tx.prepare_cached("SELECT id, title FROM places WHERE url = ?1").map_err(sql_err)?;
            let mut insert = tx.prepare_cached("INSERT INTO places(url, title) VALUES(?1, ?2)").map_err(sql_err)?;
            let mut retitle = tx.prepare_cached("UPDATE places SET title = ?2 WHERE id = ?1").map_err(sql_err)?;
            let mut visit = tx
                .prepare_cached("INSERT OR IGNORE INTO visits(place_id, time, transition, import_id) VALUES(?1, ?2, ?3, ?4)")
                .map_err(sql_err)?;
            for (i, place) in places.iter().enumerate() {
                if i % 2000 == 0 {
                    progress(i);
                }
                let Some(url) = clean_url(&place.url) else {
                    stats.urls_skipped += 1;
                    continue;
                };
                let visits: Vec<&(i64, i64)> = place.visits.iter().filter(|(t, _)| *t >= since && *t > 0).collect();
                if visits.is_empty() {
                    continue;
                }
                let title = clean_title(&place.title);
                let existing: Option<(i64, String)> =
                    find.query_row([&url], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(sql_err)?;
                let id = match existing {
                    Some((id, old)) => {
                        if old.is_empty() && !title.is_empty() {
                            retitle.execute(params![id, title]).map_err(sql_err)?;
                        }
                        id
                    }
                    None => {
                        insert.execute(params![url, title]).map_err(sql_err)?;
                        stats.places_added += 1;
                        tx.last_insert_rowid()
                    }
                };
                for (time, transition) in visits {
                    if visit.execute(params![id, time, transition, import_id]).map_err(sql_err)? > 0 {
                        stats.visits_added += 1;
                    } else {
                        stats.visits_skipped += 1;
                    }
                }
            }
            progress(places.len());
        }
        tx.execute("UPDATE imports SET visits = ?2 WHERE id = ?1", params![import_id, stats.visits_added as i64])
            .map_err(sql_err)?;
        tx.execute(
            "UPDATE places SET
                visit_count = (SELECT COUNT(*) FROM visits WHERE place_id = places.id),
                typed_count = (SELECT COUNT(*) FROM visits WHERE place_id = places.id AND transition = 1),
                last_visit = (SELECT MAX(time) FROM visits WHERE place_id = places.id)
             WHERE id IN (SELECT DISTINCT place_id FROM visits WHERE import_id = ?1)",
            [import_id],
        )
        .map_err(sql_err)?;
        tx.commit().map_err(sql_err)?;
        Ok(stats)
    }

    /// The most recent import, if its visits are still there.
    pub fn last_import(&self) -> Result<Option<ImportRecord>, String> {
        self.conn
            .query_row("SELECT id, source, time, visits FROM imports ORDER BY id DESC LIMIT 1", [], |r| {
                Ok(ImportRecord { id: r.get(0)?, source: r.get(1)?, time: r.get(2)?, visits: r.get(3)? })
            })
            .optional()
            .map_err(sql_err)
    }

    /// Removes the visits import `id` added (visits recorded since stay).
    /// Returns how many.
    pub fn undo_import(&self, id: i64) -> Result<usize, String> {
        let n = self.conn.execute("DELETE FROM visits WHERE import_id = ?1", [id]).map_err(sql_err)?;
        self.conn.execute("DELETE FROM imports WHERE id = ?1", [id]).map_err(sql_err)?;
        self.fix_places()?;
        Ok(n)
    }

    /// Writes a consistent copy of the database to `path` (replacing it).
    pub fn backup_to(&self, path: &Path) -> Result<(), String> {
        let _ = std::fs::remove_file(path);
        let target = path.to_str().ok_or("backup path is not UTF-8")?;
        self.conn.execute("VACUUM INTO ?1", [target]).map_err(sql_err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

fn hit_row(r: &rusqlite::Row) -> rusqlite::Result<Hit> {
    Ok(Hit { url: r.get(0)?, title: r.get(1)?, visit_count: r.get(2)?, last_visit: r.get(3)? })
}

/// `%term%` for LIKE, with the wildcards in `term` escaped.
fn like_pattern(term: &str) -> String {
    let mut out = String::from("%");
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// `(url LIKE ?k OR title LIKE ?k) AND ...` for `n` terms, numbered from
/// `first`.
fn term_filter(n: usize, url: &str, title: &str, first: usize) -> String {
    (0..n)
        .map(|i| {
            let k = first + i;
            format!("({url} LIKE ?{k} ESCAPE '\\' OR {title} LIKE ?{k} ESCAPE '\\')")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// `example.com/path` for `https://www.example.com/path`, lower case.
fn site_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.trim_start_matches("www.").to_lowercase()
}

// ---- the shared store: a writer thread and a reader connection ----

type Job = Box<dyn FnOnce(&mut Db) + Send>;

enum Msg {
    Visit { url: String, title: String, time: i64 },
    Title { url: String, title: String },
    Run(Job),
}

struct Shared {
    tx: Mutex<Option<Sender<Msg>>>,
    reader: Mutex<Option<Db>>,
    error: Mutex<Option<String>>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared { tx: Mutex::new(None), reader: Mutex::new(None), error: Mutex::new(None) })
}

/// Where the database lives.
pub fn db_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        static DIR: OnceLock<Option<tempfile::TempDir>> = OnceLock::new();
        if let Some(dir) = DIR.get_or_init(|| tempfile::tempdir().ok()) {
            return Ok(dir.path().join(FILE_NAME));
        }
    }
    Ok(crate::storage::data_dir()?.join(FILE_NAME))
}

fn set_error(error: Option<String>) {
    *shared().error.lock().unwrap_or_else(|e| e.into_inner()) = error;
}

/// The writer channel, starting the writer thread on first use.
fn sender() -> Option<Sender<Msg>> {
    let mut tx = shared().tx.lock().unwrap_or_else(|e| e.into_inner());
    if tx.is_none() {
        let path = match db_path() {
            Ok(path) => path,
            Err(e) => {
                set_error(Some(e));
                return None;
            }
        };
        let (send, recv) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("history".into()).spawn(move || writer(path, recv));
        if let Err(e) = spawned {
            set_error(Some(format!("could not start the history writer: {e}")));
            return None;
        }
        *tx = Some(send);
    }
    tx.clone()
}

fn writer(path: PathBuf, recv: Receiver<Msg>) {
    let mut db = match Db::open(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("browsecraft: history is not saved: {e}");
            set_error(Some(e));
            // Keep answering jobs (with nothing) so callers never hang.
            for msg in recv {
                drop(msg);
            }
            return;
        }
    };
    let mut last: Option<(String, i64)> = None;
    for msg in recv {
        let result = match msg {
            Msg::Visit { url, title, time } => {
                if last.as_ref().is_some_and(|(u, t)| *u == url && time - t < SAME_VISIT_US) {
                    db.retitle(&url, &title)
                } else {
                    last = Some((url.clone(), time));
                    db.add_visit(&url, &title, time, TRANSITION_LINK).map(|_| ())
                }
            }
            Msg::Title { url, title } => db.retitle(&url, &title),
            Msg::Run(job) => {
                last = None;
                job(&mut db);
                Ok(())
            }
        };
        if let Err(e) = result {
            eprintln!("browsecraft: {e}");
        }
    }
}

/// Runs `job` on the writer's connection and waits (at most 30 s) for its
/// result. `None` when history is unavailable.
fn run_writer<T: Send + 'static>(job: impl FnOnce(&mut Db) -> T + Send + 'static) -> Option<T> {
    let tx = sender()?;
    let (done, wait) = mpsc::channel();
    tx.send(Msg::Run(Box::new(move |db| {
        let _ = done.send(job(db));
    })))
    .ok()?;
    wait.recv_timeout(Duration::from_secs(30)).ok()
}

/// Runs `f` with the reader connection (opened on first use).
fn with_reader<T>(f: impl FnOnce(&Db) -> Result<T, String>) -> Result<T, String> {
    let mut reader = shared().reader.lock().unwrap_or_else(|e| e.into_inner());
    if reader.is_none() {
        *reader = Some(Db::open(&db_path()?)?);
    }
    match reader.as_ref() {
        Some(db) => f(db),
        None => Err("history is unavailable".into()),
    }
}

/// Starts the writer and applies the retention settings. Call once at
/// start-up.
pub fn init() {
    let _ = sender();
    let f = crate::flags::get();
    let (days, max) = (f.history_days, f.history_max_visits);
    if let Some(tx) = sender() {
        let _ = tx.send(Msg::Run(Box::new(move |db| {
            if let Err(e) = db.enforce_retention(days, max, now_us()) {
                eprintln!("browsecraft: {e}");
            }
        })));
    }
}

/// Records a visit to `url` in a tab of `container` (temporary containers
/// are never recorded). Returns at once; the writer thread saves it.
pub fn record(url: &str, title: &str, container: Option<&str>) {
    if container.is_some_and(crate::containers::is_temporary_id) {
        return;
    }
    let Some(url) = clean_url(url) else { return };
    if let Some(tx) = sender() {
        let _ = tx.send(Msg::Visit { url, title: title.to_string(), time: now_us() });
    }
}

/// The page at `url` reported its title.
pub fn retitle(url: &str, title: &str, container: Option<&str>) {
    if container.is_some_and(crate::containers::is_temporary_id) {
        return;
    }
    let Some(url) = clean_url(url) else { return };
    if let Some(tx) = sender() {
        let _ = tx.send(Msg::Title { url, title: title.to_string() });
    }
}

/// Waits until everything sent to the writer is saved.
pub fn flush() {
    let _ = run_writer(|_| ());
}

/// Ranked search (see `Db::search`); the most recent when `query` is empty.
pub fn search(query: &str, limit: usize) -> Vec<Hit> {
    with_reader(|db| db.search(query, limit, now_us())).unwrap_or_default()
}

/// Visits newest first, filtered like `search`, from `offset` (the
/// Library sidebar's pages). The flag says whether there are more.
pub fn visits(query: &str, offset: usize, limit: usize) -> (Vec<VisitRow>, bool) {
    with_reader(|db| db.visits(query, offset, limit)).unwrap_or_default()
}

/// Forgets an address and all its visits.
pub fn remove_url(url: &str) -> Result<bool, String> {
    let url = url.to_string();
    run_writer(move |db| db.remove_url(&url)).unwrap_or_else(|| Err("history is unavailable".into()))
}

/// The local day of a visit and its "Today" / "Yesterday" / date heading.
pub fn day_heading(us: i64) -> (i64, String) {
    let day = local_day(us).0;
    (day, day_label(day, local_day(now_us()).0))
}

/// "14:05", local time.
pub fn time_of_day(us: i64) -> String {
    let secs = local_day(us).1;
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// Forgets every visit.
pub fn clear() -> Result<(), String> {
    run_writer(|db| db.clear_all()).unwrap_or_else(|| Err("history is unavailable".into()))
}

/// Forgets the visits of the last `span` microseconds.
pub fn clear_recent(span: i64) -> Result<usize, String> {
    let now = now_us();
    run_writer(move |db| db.clear_range(now - span, i64::MAX)).unwrap_or_else(|| Err("history is unavailable".into()))
}

/// Runs an import on the writer's connection (see `Db::import`), after
/// saving a copy of the database next to it (`history.sqlite.bak`).
pub fn import(
    places: Vec<ImportPlace>,
    source: String,
    since: i64,
    progress: Box<dyn FnMut(usize) + Send>,
) -> Result<ImportStats, String> {
    run_writer_long(move |db| {
        let mut progress = progress;
        if let Ok(path) = db_path() {
            db.backup_to(&path.with_extension("sqlite.bak"))?;
        }
        db.import(&places, &source, since, &mut *progress)
    })
}

/// `run_writer` without the 30 s limit (imports of large histories).
fn run_writer_long<T: Send + 'static>(job: impl FnOnce(&mut Db) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let tx = sender().ok_or("history is unavailable")?;
    let (done, wait) = mpsc::channel();
    tx.send(Msg::Run(Box::new(move |db| {
        let _ = done.send(job(db));
    })))
    .map_err(|_| "history is unavailable".to_string())?;
    wait.recv().map_err(|_| "history is unavailable".to_string())?
}

/// The last import, for its undo button.
pub fn last_import() -> Option<ImportRecord> {
    with_reader(|db| db.last_import()).ok().flatten()
}

/// Undoes import `id`.
pub fn undo_import(id: i64) -> Result<usize, String> {
    run_writer_long(move |db| db.undo_import(id))
}

/// (addresses, visits).
#[cfg(test)]
pub fn counts() -> (i64, i64) {
    with_reader(|db| db.counts()).unwrap_or((0, 0))
}

// ---- dates ----

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The local time zone's offset from UTC at `secs`, in seconds.
fn utc_offset(secs: i64) -> i64 {
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r writes only into `tm`, which outlives the call.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if !libc::localtime_r(&t, &mut tm).is_null() {
                return tm.tm_gmtoff as i64;
            }
        }
    }
    let _ = secs;
    0
}

/// Local (days since the epoch, seconds into that day) for `us`.
fn local_day(us: i64) -> (i64, i64) {
    let secs = us.div_euclid(1_000_000);
    let local = secs + utc_offset(secs);
    (local.div_euclid(86_400), local.rem_euclid(86_400))
}

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"];

/// "Today", "Yesterday" or "Monday, 9 November 2026".
fn day_label(day: i64, today: i64) -> String {
    match today - day {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let (y, m, d) = civil_from_days(day);
            let weekday = WEEKDAYS[day.rem_euclid(7) as usize];
            format!("{weekday}, {d} {} {y}", MONTHS[(m as usize).saturating_sub(1).min(11)])
        }
    }
}

/// `2026-11-09` for microseconds since the epoch (local time).
pub fn date_string(us: i64) -> String {
    let (y, m, d) = civil_from_days(local_day(us).0);
    format!("{y:04}-{m:02}-{d:02}")
}

// ---- browsecraft://history ----

static FORM_TOKEN: FormToken = FormToken::new();

/// Applies a submission of the history page (`clear=hour|day|week|month|all`
/// or `remove=<url>`); only links of a freshly rendered page carry the
/// token. Returns a message for the page.
pub fn apply_form(query: &str) -> Option<String> {
    let pairs = FORM_TOKEN.take(query)?;
    let mut message = None;
    for (k, v) in pairs {
        match (k.as_str(), v.as_str()) {
            ("clear", span) => {
                let result = match span {
                    "hour" => clear_recent(HOUR_US).map(|_| ()),
                    "day" => clear_recent(DAY_US).map(|_| ()),
                    "week" => clear_recent(7 * DAY_US).map(|_| ()),
                    "month" => clear_recent(28 * DAY_US).map(|_| ()),
                    "all" => clear(),
                    _ => continue,
                };
                message = Some(match result {
                    Ok(()) => "History cleared.".to_string(),
                    Err(e) => e,
                });
            }
            ("remove", url) => {
                let url = url.to_string();
                let result = run_writer(move |db| db.remove_url(&url)).unwrap_or_else(|| Err("history is unavailable".into()));
                message = Some(match result {
                    Ok(_) => "Removed from history.".to_string(),
                    Err(e) => e,
                });
            }
            _ => {}
        }
    }
    message
}

/// The address the history page shows for a query (no token).
pub fn page_url(query: &str) -> String {
    let mut q = String::new();
    let mut p = 0;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "q" => q = v.into_owned(),
            "p" => p = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    let mut url = String::from("browsecraft://history");
    if !q.trim().is_empty() {
        url.push_str(&format!("?q={}", internal::encode(q.trim())));
    }
    if p > 0 {
        url.push_str(if url.contains('?') { "&" } else { "?" });
        url.push_str(&format!("p={p}"));
    }
    url
}

/// The `browsecraft://history` page for `query` (`q`: search, `p`: page),
/// visits newest first, grouped by day.
pub fn page_html(query: &str, message: Option<&str>) -> String {
    let token = match FORM_TOKEN.issue() {
        Ok(token) => token,
        Err(error) => return internal::unavailable_page("History", &error),
    };
    let mut q = String::new();
    let mut page: usize = 0;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "q" => q = v.chars().take(200).collect(),
            "p" => page = v.parse().unwrap_or(0).min(100_000),
            _ => {}
        }
    }
    let q = q.trim().to_string();
    let result = with_reader(|db| Ok((db.visits(&q, page * PAGE_SIZE, PAGE_SIZE)?, db.counts()?)));
    let error = shared().error.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut body = String::from("<h1>History</h1>");
    let ((rows, more), (places, visits)) = match result {
        Ok(r) => r,
        Err(e) => {
            body.push_str(&format!("<p class=\"sub bad\">{}</p>", internal::escape(&error.unwrap_or(e))));
            return internal::page("History", &body);
        }
    };
    let retention = match crate::flags::get().history_days {
        0 => "kept until you clear it".to_string(),
        n => format!("kept for {n} days"),
    };
    body.push_str(&format!(
        "<p class=\"sub\">{visits} visits to {places} pages, {retention}. Stored on this computer only.</p>"
    ));
    if let Some(message) = message {
        body.push_str(&format!("<p class=\"ok\">{}</p>", internal::escape(message)));
    }
    body.push_str(&format!(
        "<form method=\"get\" action=\"browsecraft://history\"><input class=\"field\" type=\"search\" name=\"q\" \
         value=\"{}\" placeholder=\"Search history\"></form>",
        internal::escape(&q)
    ));
    let clear = |span: &str, label: &str| {
        format!("<a class=\"btn ghost\" href=\"browsecraft://history?token={token}&amp;clear={span}\">{label}</a> ")
    };
    body.push_str(&format!(
        "<div class=\"card\"><div class=\"row first\"><div class=\"grow\"><div class=\"t\">Clear history</div>\
         <div class=\"d\">Ctrl + Shift + Delete also clears cookies and site data.</div></div></div>\
         <div class=\"note\">{}{}{}{}<a class=\"btn danger\" href=\"browsecraft://history?token={token}&amp;clear=all\">Everything</a> \
         <a class=\"btn ghost\" href=\"browsecraft://import\">Import…</a></div></div>",
        clear("hour", "Last hour"),
        clear("day", "Last 24 hours"),
        clear("week", "Last 7 days"),
        clear("month", "Last 4 weeks"),
    ));
    if rows.is_empty() {
        let text = if q.is_empty() { "No history yet. Pages you visit will show up here." } else { "Nothing in your history matches." };
        body.push_str(&format!("<div class=\"card\"><div class=\"empty\">{text}</div></div>"));
    }
    let today = local_day(now_us()).0;
    let mut current_day = None;
    for row in &rows {
        let (day, secs) = local_day(row.time);
        if current_day != Some(day) {
            if current_day.is_some() {
                body.push_str("</div>");
            }
            body.push_str(&format!("<div class=\"card\"><h2>{}</h2>", internal::escape(&day_label(day, today))));
            current_day = Some(day);
        }
        let title = if row.title.is_empty() { row.url.clone() } else { row.title.clone() };
        body.push_str(&format!(
            "<div class=\"item\"><div class=\"ico\" style=\"width: 52px; font-size: 12px\">{h:02}:{m:02}</div>\
             <div class=\"main\"><a class=\"name\" href=\"{url}\">{title}</a><div class=\"url\">{url}</div></div>\
             <a class=\"btn ghost\" href=\"browsecraft://history?token={token}&amp;remove={enc}\">Remove</a></div>",
            h = secs / 3600,
            m = secs % 3600 / 60,
            url = internal::escape(&row.url),
            title = internal::escape(&title),
            enc = internal::escape(&internal::encode(&row.url)),
        ));
    }
    if current_day.is_some() {
        body.push_str("</div>");
    }
    let link = |p: usize| {
        let mut href = String::from("browsecraft://history?");
        if !q.is_empty() {
            href.push_str(&format!("q={}&amp;", internal::escape(&internal::encode(&q))));
        }
        href.push_str(&format!("p={p}"));
        href
    };
    let mut nav = String::new();
    if page > 0 {
        nav.push_str(&format!("<a class=\"btn ghost\" href=\"{}\">Newer</a> ", link(page - 1)));
    }
    if more {
        nav.push_str(&format!("<a class=\"btn ghost\" href=\"{}\">Older</a>", link(page + 1)));
    }
    if !nav.is_empty() {
        body.push_str(&format!("<div class=\"note\">{nav}</div>"));
    }
    internal::page("History", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000_000;

    #[test]
    fn records_visits_and_counts() {
        let db = Db::open_memory();
        assert!(db.add_visit("https://a.test/", "A", NOW - 10, TRANSITION_LINK).unwrap());
        assert!(!db.add_visit("https://a.test/", "", NOW - 10, TRANSITION_LINK).unwrap(), "same time is one visit");
        assert!(db.add_visit("https://a.test/", "", NOW, TRANSITION_TYPED).unwrap());
        let hits = db.search("", 10, NOW).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "A", "an empty title keeps the known one");
        assert_eq!(hits[0].visit_count, 2);
        assert_eq!(hits[0].last_visit, NOW);
        assert_eq!(db.counts().unwrap(), (1, 2));
        db.retitle("https://a.test/", "  New\ttitle ").unwrap();
        assert_eq!(db.search("new", 1, NOW).unwrap()[0].title, "New title");
    }

    #[test]
    fn search_ranks_frequent_and_recent_first() {
        let db = Db::open_memory();
        // Visited once, today.
        db.add_visit("https://news.test/once", "Rust news", NOW - HOUR_US, 0).unwrap();
        // Visited often, recently.
        for i in 0..10 {
            db.add_visit("https://blog.test/rust", "A blog about rust", NOW - i * HOUR_US - 5, 0).unwrap();
        }
        // Visited often, long ago.
        for i in 0..10 {
            db.add_visit("https://old.test/rust", "Old rust page", NOW - 400 * DAY_US - i, 0).unwrap();
        }
        // Site starts with the query: boosted.
        db.add_visit("https://rust-lang.org/", "Home", NOW - 2 * DAY_US, 0).unwrap();
        let urls: Vec<String> = db.search("rust", 10, NOW).unwrap().into_iter().map(|h| h.url).collect();
        assert_eq!(urls[0], "https://blog.test/rust");
        assert_eq!(urls.last().map(String::as_str), Some("https://old.test/rust"));
        assert!(urls.contains(&"https://rust-lang.org/".to_string()));
        // Every word must match, in the title or the URL, any case.
        let hits = db.search("BLOG rust", 10, NOW).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(db.search("nothing-like-this", 10, NOW).unwrap().is_empty());
        // LIKE wildcards are literal.
        assert!(db.search("%", 10, NOW).unwrap().is_empty());
        assert!(db.search("_", 10, NOW).unwrap().is_empty());
    }

    #[test]
    fn clearing_ranges_and_urls() {
        let db = Db::open_memory();
        db.add_visit("https://old.test/", "Old", NOW - 10 * DAY_US, 0).unwrap();
        db.add_visit("https://both.test/", "Both", NOW - 10 * DAY_US, 0).unwrap();
        db.add_visit("https://both.test/", "Both", NOW - HOUR_US / 2, 0).unwrap();
        db.add_visit("https://new.test/", "New", NOW - 60, 0).unwrap();
        assert_eq!(db.clear_range(NOW - HOUR_US, i64::MAX).unwrap(), 2);
        let hits = db.search("", 10, NOW).unwrap();
        assert_eq!(hits.len(), 2, "new.test had no other visit and is gone");
        let both = hits.iter().find(|h| h.url == "https://both.test/").unwrap();
        assert_eq!((both.visit_count, both.last_visit), (1, NOW - 10 * DAY_US));
        assert!(db.remove_url("https://old.test/").unwrap());
        assert_eq!(db.counts().unwrap(), (1, 1));
        db.clear_all().unwrap();
        assert_eq!(db.counts().unwrap(), (0, 0));
    }

    #[test]
    fn retention_by_age_and_count() {
        let db = Db::open_memory();
        for i in 0..20 {
            db.add_visit(&format!("https://r{i}.test/"), "", NOW - i * DAY_US, 0).unwrap();
        }
        assert_eq!(db.enforce_retention(10, 0, NOW).unwrap(), 9, "older than 10 days");
        assert_eq!(db.counts().unwrap(), (11, 11));
        assert_eq!(db.enforce_retention(0, 5, NOW).unwrap(), 6);
        let urls: Vec<String> = db.search("", 10, NOW).unwrap().into_iter().map(|h| h.url).collect();
        assert_eq!(urls, ["https://r0.test/", "https://r1.test/", "https://r2.test/", "https://r3.test/", "https://r4.test/"]);
        assert_eq!(db.enforce_retention(0, 0, NOW).unwrap(), 0);
    }

    #[test]
    fn import_merges_dedups_and_undoes() {
        let mut db = Db::open_memory();
        db.add_visit("https://a.test/", "", NOW - 100, 0).unwrap();
        let places = vec![
            ImportPlace { url: "https://a.test/".into(), title: "A".into(), visits: vec![(NOW - 100, 0), (NOW - 200, 1)] },
            ImportPlace { url: "https://b.test/".into(), title: "B".into(), visits: vec![(NOW - 300, 0), (NOW - 300, 0)] },
            ImportPlace { url: "https://old.test/".into(), title: "".into(), visits: vec![(NOW - 50 * DAY_US, 0)] },
            ImportPlace { url: "javascript:alert(1)".into(), title: "".into(), visits: vec![(NOW, 0)] },
            ImportPlace { url: "https://u:p@creds.test/".into(), title: "".into(), visits: vec![(NOW - 5, 0)] },
        ];
        let mut ticks = 0;
        let stats = db.import(&places, "test", NOW - 30 * DAY_US, &mut |_| ticks += 1).unwrap();
        assert!(ticks >= 1);
        assert_eq!(stats.places_added, 2, "b.test and creds.test");
        assert_eq!(stats.visits_added, 3);
        assert_eq!(stats.visits_skipped, 2, "(a, NOW-100) existed; b's duplicate");
        assert_eq!(stats.urls_skipped, 1);
        let a = db.search("a.test", 1, NOW).unwrap().remove(0);
        assert_eq!((a.title.as_str(), a.visit_count), ("A", 2), "empty title filled in, counts merged");
        assert!(db.search("old.test", 1, NOW).unwrap().is_empty(), "before the range");
        assert!(db.search("creds", 1, NOW).unwrap()[0].url == "https://creds.test/");
        // The same import again adds nothing.
        let again = db.import(&places, "test", NOW - 30 * DAY_US, &mut |_| {}).unwrap();
        assert_eq!(again.visits_added, 0);
        let last = db.last_import().unwrap().unwrap();
        assert_eq!(last.id, again.import_id);
        assert_eq!(db.undo_import(again.import_id).unwrap(), 0);
        let first = db.last_import().unwrap().unwrap();
        assert_eq!(first.id, stats.import_id);
        assert_eq!(db.undo_import(first.id).unwrap(), 3);
        // The visit recorded while browsing stays.
        assert_eq!(db.counts().unwrap(), (1, 1));
        assert_eq!(db.search("", 5, NOW).unwrap()[0].visit_count, 1);
    }

    #[test]
    fn visits_page_newest_first_with_paging() {
        let db = Db::open_memory();
        for i in 0..5 {
            db.add_visit(&format!("https://p{i}.test/"), &format!("Page {i}"), NOW - i, 0).unwrap();
        }
        let (rows, more) = db.visits("", 0, 3).unwrap();
        assert!(more);
        assert_eq!(rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["Page 0", "Page 1", "Page 2"]);
        let (rows, more) = db.visits("", 3, 3).unwrap();
        assert!(!more);
        assert_eq!(rows.len(), 2);
        assert_eq!(db.visits("page 4", 0, 10).unwrap().0.len(), 1);
    }

    #[test]
    fn urls_are_cleaned() {
        assert_eq!(clean_url("https://alice:secret@x.test/a").as_deref(), Some("https://x.test/a"));
        for skipped in ["browsecraft://history", "about:blank", "view-source:https://x.test/", "javascript:x", "data:text/html,x", "", "place:sort=8"] {
            assert_eq!(clean_url(skipped), None, "{skipped}");
        }
        assert_eq!(clean_url(&format!("https://long.test/?{}", "x".repeat(MAX_URL_LEN))), None);
        assert_eq!(clean_title(&"t".repeat(1000)).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn file_is_private_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("history.sqlite");
        {
            let db = Db::open(&path).unwrap();
            db.add_visit("https://disk.test/", "Disk", NOW, 0).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.search("disk", 1, NOW).unwrap()[0].title, "Disk");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let backup = dir.path().join("copy.sqlite");
        db.backup_to(&backup).unwrap();
        assert_eq!(Db::open(&backup).unwrap().counts().unwrap(), (1, 1));
    }

    #[test]
    fn dates_and_labels() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_401), (2025, 11, 9));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(day_label(100, 100), "Today");
        assert_eq!(day_label(99, 100), "Yesterday");
        assert_eq!(day_label(20_401, 20_500), "Sunday, 9 November 2025");
    }

    /// The global store (writer thread and reader): temporary containers
    /// are skipped, title updates do not add visits, the page renders.
    #[test]
    fn global_store_records_and_skips_temporary_containers() {
        record("https://global.test/a", "First", None);
        record("https://global.test/a", "Second", None);
        retitle("https://global.test/a", "Final", None);
        record("https://secret-temp.test/", "Temp", Some("tmp-abc"));
        retitle("https://secret-temp.test/", "Temp", Some("tmp-abc"));
        record("https://container.test/", "Work", Some("work"));
        record("browsecraft://history", "History", None);
        flush();
        let hits = search("global.test", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].title.as_str(), hits[0].visit_count), ("Final", 1));
        assert!(search("secret-temp", 5).is_empty());
        assert_eq!(search("container.test", 5).len(), 1);
        let html = page_html("q=global", None);
        assert!(html.contains("Final") && html.contains("Today"));
        assert!(!html.contains("secret-temp"));
        let html = page_html("q=%3Cb%3E", None);
        assert!(html.contains("&lt;b&gt;"), "the query is escaped");
        assert_eq!(page_url("q=a%20b&p=2&token=x"), "browsecraft://history?q=a+b&p=2");
    }
}
