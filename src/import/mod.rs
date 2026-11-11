//! Importing bookmarks and history from Firefox and the Chromium family,
//! and bookmarks from (and to) the Netscape `bookmarks.html` format.
//!
//! Safety: another browser's profile is never written. Its SQLite
//! databases are copied (with their `-wal` / `-journal` files, which hold
//! the newest data while that browser runs) into a private temporary
//! folder and the copy is opened; Chromium's `Bookmarks` is a plain JSON
//! file read directly. Every input is size-limited.
//!
//! Only bookmarks and history: passwords (Firefox encrypts them with NSS,
//! Chrome with the desktop keyring), cookies and extensions are not
//! imported.
//!
//! The browsecraft://import page (`page.rs`) runs an import on a worker
//! thread and shows its progress; `browsecraft import` (`cli.rs`) runs it
//! from a terminal, also while the browser is open: history is SQLite, and
//! bookmarks.json is changed under its lock file and reloaded by the
//! running browser.

pub mod chromium;
pub mod cli;
pub mod detect;
pub mod firefox;
pub mod html;
pub mod page;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::bookmarks::{self, Destination, ImportedBookmarks, MergeStats};
use crate::history::{self, ImportPlace, ImportStats};
use detect::{Family, Profile};

/// The largest database file copied.
const MAX_DB_FILE: u64 = 8 << 30;
/// Most visits read from one profile (the newest first).
pub const MAX_VISITS: usize = 2_000_000;
const DAY_US: i64 = 86_400_000_000;

/// What a profile holds, for the list of profiles.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub bookmarks: usize,
    /// Addresses in the history.
    pub pages: usize,
    pub visits: usize,
    /// Microseconds since the Unix epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_visit: Option<i64>,
}

/// A read-only copy of another browser's database, deleted when dropped.
pub struct DbCopy {
    pub conn: Connection,
    _dir: tempfile::TempDir,
}

/// Copies `path` (and its `-wal` and `-journal` files) into a new private
/// temporary folder and opens the copy. The source is only read, so this
/// works while the other browser runs and holds its locks; its `-shm`
/// index is not copied (SQLite rebuilds it from the copied journal).
pub fn open_copy(path: &Path) -> Result<DbCopy, String> {
    let name = path.file_name().ok_or("not a file")?.to_string_lossy().into_owned();
    let dir = tempfile::Builder::new()
        .prefix("browsecraft-import-")
        .tempdir()
        .map_err(|e| format!("could not create a temporary folder: {e}"))?;
    for suffix in ["", "-wal", "-journal"] {
        let from = PathBuf::from(format!("{}{suffix}", path.display()));
        let meta = match std::fs::metadata(&from) {
            Ok(meta) => meta,
            Err(e) if suffix.is_empty() => return Err(format!("{}: {e}", from.display())),
            Err(_) => continue,
        };
        if !meta.is_file() || meta.len() > MAX_DB_FILE {
            return Err(format!("{} is not a file this import can read", from.display()));
        }
        let to = dir.path().join(format!("{name}{suffix}"));
        std::fs::copy(&from, &to).map_err(|e| format!("could not copy {}: {e}", from.display()))?;
    }
    let copy = dir.path().join(&name);
    // Read-only first; a copied hot journal or WAL may need the copy
    // opened for writing so SQLite can recover it (the source is untouched).
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let check = |conn: Connection| -> Result<Connection, rusqlite::Error> {
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))?;
        Ok(conn)
    };
    let conn = Connection::open_with_flags(&copy, flags)
        .and_then(check)
        .or_else(|_| Connection::open(&copy).and_then(check))
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    let _ = conn.execute_batch("PRAGMA query_only = 1");
    Ok(DbCopy { conn, _dir: dir })
}

/// Where to import from.
#[derive(Clone, Debug)]
pub enum Source {
    Profile(Profile),
    Html(PathBuf),
}

impl Source {
    /// "Firefox – default-release", "bookmarks.html".
    pub fn label(&self) -> String {
        match self {
            Source::Profile(p) => p.label(),
            Source::Html(path) => path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
        }
    }

    /// The folder imported bookmarks go to (unless merged).
    pub fn folder_name(&self) -> String {
        match self {
            Source::Profile(p) => {
                let browser = p.browser.split(" (").next().unwrap_or(&p.browser);
                format!("Imported from {browser} ({})", p.name)
            }
            Source::Html(_) => format!("Imported from {}", self.label()),
        }
    }
}

/// What to import.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub bookmarks: bool,
    pub history: bool,
    /// Only history of the last this many days (`None`: all, within the
    /// retention setting).
    pub days: Option<u32>,
    /// Bookmarks: the other bar into ours and the rest into "Other
    /// bookmarks", instead of an "Imported from ..." folder.
    pub merge: bool,
    /// Count only; change nothing.
    pub dry_run: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { bookmarks: true, history: true, days: None, merge: false, dry_run: false }
    }
}

/// What an import found and did, also printed as JSON by the command line.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub source: String,
    pub dry_run: bool,
    /// History before this date (local, YYYY-MM-DD) is not imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_since: Option<String>,
    pub bookmarks_found: usize,
    pub folders_found: usize,
    pub history_pages_found: usize,
    pub history_visits_found: usize,
    /// What the bookmarks import added (would add, in a dry run).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmarks: Option<MergeStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<ImportStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    pub errors: Vec<String>,
    pub seconds: f64,
}

impl Report {
    /// One line for a notice.
    pub fn line(&self) -> String {
        let verb = if self.dry_run { "would add" } else { "added" };
        let mut parts = Vec::new();
        if let Some(b) = &self.bookmarks {
            parts.push(format!("{} bookmarks {verb} ({} already there)", b.added, b.duplicates));
        }
        if let Some(h) = &self.history {
            parts.push(format!("{} history visits added ({} already there)", h.visits_added, h.visits_skipped));
        } else if self.history_visits_found > 0 {
            parts.push(format!("{} history visits found", self.history_visits_found));
        }
        if parts.is_empty() && self.errors.is_empty() {
            parts.push("nothing to import".into());
        }
        parts.extend(self.errors.iter().cloned());
        format!("{}: {}", self.source, parts.join("; "))
    }
}

/// The oldest visit to import: the last `days` days, never older than the
/// history retention setting (it would be deleted at the next start).
pub fn history_since(days: Option<u32>, now: i64) -> i64 {
    let keep = crate::flags::get().history_days;
    let mut since = 0;
    if let Some(d) = days.filter(|d| *d > 0) {
        since = now - i64::from(d) * DAY_US;
    }
    if keep > 0 {
        since = since.max(now - i64::from(keep) * DAY_US);
    }
    since
}

/// Reads the bookmarks of `source`.
pub fn read_bookmarks(source: &Source) -> Result<ImportedBookmarks, String> {
    match source {
        Source::Html(path) => html::parse_file(path),
        Source::Profile(p) => match p.family {
            Family::Firefox => firefox::bookmarks(&open_copy(&p.dir.join("places.sqlite"))?.conn),
            Family::Chromium => {
                let file = p.dir.join("Bookmarks");
                if file.is_file() { chromium::bookmarks_file(&file) } else { Ok(ImportedBookmarks::default()) }
            }
        },
    }
}

/// Reads the history of `source` since `since` (microseconds).
pub fn read_history(source: &Source, since: i64) -> Result<Vec<ImportPlace>, String> {
    match source {
        Source::Html(_) => Ok(Vec::new()),
        Source::Profile(p) => match p.family {
            Family::Firefox => firefox::history(&open_copy(&p.dir.join("places.sqlite"))?.conn, since, MAX_VISITS),
            Family::Chromium => {
                let file = p.dir.join("History");
                if file.is_file() { chromium::history(&open_copy(&file)?.conn, since, MAX_VISITS) } else { Ok(Vec::new()) }
            }
        },
    }
}

/// The files a profile's summary depends on, with their modification
/// times and sizes.
fn stamp(p: &Profile) -> Vec<(SystemTime, u64)> {
    let files: &[&str] = match p.family {
        Family::Firefox => &["places.sqlite", "places.sqlite-wal"],
        Family::Chromium => &["Bookmarks", "History", "History-journal"],
    };
    files
        .iter()
        .filter_map(|f| std::fs::metadata(p.dir.join(f)).ok())
        .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
        .collect()
}

type SummaryCache = HashMap<PathBuf, (Vec<(SystemTime, u64)>, Summary)>;

/// What `p` holds (cached until its files change).
pub fn summary(p: &Profile) -> Result<Summary, String> {
    static CACHE: OnceLock<Mutex<SummaryCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let now = stamp(p);
    if let Some((s, summary)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&p.dir)
        && *s == now
    {
        return Ok(summary.clone());
    }
    let summary = match p.family {
        Family::Firefox => firefox::summary(&open_copy(&p.dir.join("places.sqlite"))?.conn)?,
        Family::Chromium => {
            let history = p.dir.join("History");
            let mut s = if history.is_file() { chromium::history_summary(&open_copy(&history)?.conn)? } else { Summary::default() };
            let marks = p.dir.join("Bookmarks");
            if marks.is_file() {
                s.bookmarks = chromium::bookmarks_file(&marks)?.counts().0;
            }
            s
        }
    };
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(p.dir.clone(), (now, summary.clone()));
    Ok(summary)
}

/// Progress messages for the user.
pub type Progress = Arc<dyn Fn(String) + Send + Sync>;

/// Runs an import (or a dry run). Errors of one part (say, a locked or
/// damaged history) are reported and the other part still runs.
pub fn run(source: &Source, options: &Options, progress: Progress) -> Report {
    let started = Instant::now();
    let mut report = Report { source: source.label(), dry_run: options.dry_run, ..Default::default() };
    if options.bookmarks {
        progress(format!("Reading the bookmarks of {}…", source.label()));
        match read_bookmarks(source) {
            Ok(found) => {
                (report.bookmarks_found, report.folders_found) = found.counts();
                let dest = if options.merge { Destination::Merge } else { Destination::Folder(source.folder_name()) };
                report.destination = Some(match &dest {
                    Destination::Merge => format!("{} and {}", bookmarks::BAR_TITLE, bookmarks::OTHER_TITLE),
                    Destination::Folder(name) => format!("{} \u{203a} {name}", bookmarks::OTHER_TITLE),
                });
                if report.bookmarks_found > 0 {
                    let result = if options.dry_run {
                        let mut tree = (*bookmarks::snapshot()).clone();
                        Ok(bookmarks::merge_import(&mut tree, &found, &dest, &source.label()))
                    } else {
                        bookmarks::import(&found, &dest, &source.label())
                    };
                    match result {
                        Ok(stats) => report.bookmarks = Some(stats),
                        Err(e) => report.errors.push(format!("bookmarks: {e}")),
                    }
                }
            }
            Err(e) => report.errors.push(format!("bookmarks: {e}")),
        }
    }
    if options.history && matches!(source, Source::Profile(_)) {
        let since = history_since(options.days, history::now_us());
        if since > 0 {
            report.history_since = Some(history::date_string(since));
        }
        progress(format!("Reading the history of {}…", source.label()));
        match read_history(source, since) {
            Ok(places) => {
                report.history_pages_found = places.len();
                report.history_visits_found = places.iter().map(|p| p.visits.len()).sum();
                if !options.dry_run && !places.is_empty() {
                    let total = places.len();
                    let p = progress.clone();
                    let tick = Box::new(move |done: usize| p(format!("Importing history: {done} of {total} pages…")));
                    match history::import(places, source.label(), since, tick) {
                        Ok(stats) => report.history = Some(stats),
                        Err(e) => report.errors.push(format!("history: {e}")),
                    }
                }
            }
            Err(e) => report.errors.push(format!("history: {e}")),
        }
    }
    report.seconds = (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    report
}

/// Undoes the last import: its bookmarks and its history visits. Returns
/// a message.
pub fn undo_last() -> Result<String, String> {
    let mut parts = Vec::new();
    match bookmarks::undo_last_import() {
        Ok(n) => parts.push(format!("{n} bookmarks and folders removed")),
        Err(e) if e.contains("no import") => {}
        Err(e) => return Err(e),
    }
    if let Some(last) = history::last_import() {
        let n = history::undo_import(last.id)?;
        parts.push(format!("{n} history visits removed"));
    }
    if parts.is_empty() {
        return Err("There is no import to undo.".into());
    }
    Ok(format!("Import undone: {}.", parts.join(", ")))
}

/// Writes all bookmarks to `dir/bookmarks-<date>.html` (never replacing a
/// file). Returns the path.
pub fn export_to(dir: &Path) -> Result<PathBuf, String> {
    let text = html::export(&bookmarks::snapshot());
    let date = history::date_string(history::now_us());
    for n in 1..100 {
        let name = if n == 1 { format!("bookmarks-{date}.html") } else { format!("bookmarks-{date}-{n}.html") };
        let path = dir.join(name);
        match crate::storage::create_new(&path, text.as_bytes(), true) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Could not write {}: {e}", path.display())),
        }
    }
    Err("Too many bookmark exports today.".into())
}

/// `~/Downloads` (created if needed), else the home folder.
pub fn downloads_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).ok_or("no home folder")?;
    let dir = home.join("Downloads");
    if dir.is_dir() || std::fs::create_dir_all(&dir).is_ok() { Ok(dir) } else { Ok(home) }
}

// ---- the import running in the browser ----

/// What the browser is told while an import runs.
#[derive(Clone, Debug)]
pub enum Event {
    /// The job's progress text changed (see `job()`).
    Progress,
    /// Finished: the one-line summary.
    Done(String),
}

/// The import started from browsecraft://import.
#[derive(Default)]
pub struct Job {
    pub running: bool,
    pub progress: String,
    pub report: Option<Report>,
}

pub fn job() -> std::sync::MutexGuard<'static, Job> {
    static JOB: OnceLock<Mutex<Job>> = OnceLock::new();
    JOB.get_or_init(|| Mutex::new(Job::default())).lock().unwrap_or_else(|e| e.into_inner())
}

type Notifier = Box<dyn Fn(Event) + Send + Sync>;
static NOTIFIER: OnceLock<Notifier> = OnceLock::new();

/// How the browser hears about an import's progress (set once).
pub fn set_notifier(f: impl Fn(Event) + Send + Sync + 'static) {
    let _ = NOTIFIER.set(Box::new(f));
}

fn notify(event: Event) {
    if let Some(f) = NOTIFIER.get() {
        f(event);
    }
}

/// Starts an import on a worker thread. Fails if one is running.
pub fn start(source: Source, options: Options) -> Result<(), String> {
    {
        let mut job = job();
        if job.running {
            return Err("An import is already running.".into());
        }
        job.running = true;
        job.progress = format!("Starting the import from {}…", source.label());
        job.report = None;
    }
    let last = Arc::new(Mutex::new(Instant::now()));
    let progress: Progress = Arc::new(move |text: String| {
        job().progress = text.clone();
        // At most a few page refreshes a second.
        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed().as_millis() >= 400 {
            *last = Instant::now();
            notify(Event::Progress);
        }
    });
    let spawned = std::thread::Builder::new().name("import".into()).spawn(move || {
        let report = run(&source, &options, progress);
        let line = report.line();
        {
            let mut job = job();
            job.running = false;
            job.report = Some(report);
        }
        notify(Event::Done(line));
    });
    if let Err(e) = spawned {
        job().running = false;
        return Err(format!("could not start the import: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_are_read_while_the_source_is_locked_and_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.sqlite");
        let db = firefox::fixture(&path);
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;").unwrap();
        db.execute("INSERT INTO moz_places(id, url, title) VALUES (1, 'https://wal.test/', 'Only in the WAL')", []).unwrap();
        db.execute("INSERT INTO moz_bookmarks(id, type, fk, parent, position, title) VALUES (50, 1, 1, 3, 0, 'In WAL')", []).unwrap();
        // Hold an exclusive lock, as a running Firefox does.
        db.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;").unwrap();
        assert!(dir.path().join("places.sqlite-wal").exists());
        let before: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
            .collect();
        let copy = open_copy(&path).unwrap();
        let b = firefox::bookmarks(&copy.conn).unwrap();
        assert_eq!(b.bar.len(), 1, "the WAL's data is in the copy");
        assert!(copy.conn.execute("DELETE FROM moz_places", []).is_err(), "the copy is query-only");
        let tmp = copy._dir.path().to_path_buf();
        drop(copy);
        assert!(!tmp.exists(), "the copy is deleted");
        let after: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
            .collect();
        assert_eq!(before, after, "the source files are unchanged");
        drop(db);
        assert!(open_copy(&dir.path().join("missing.sqlite")).is_err());
        std::fs::write(dir.path().join("junk.sqlite"), b"not a database at all, just text").unwrap();
        assert!(open_copy(&dir.path().join("junk.sqlite")).is_err());
    }

    fn firefox_profile(root: &Path) -> Profile {
        let dir = root.join("abc.default");
        std::fs::create_dir_all(&dir).unwrap();
        let db = firefox::fixture(&dir.join("places.sqlite"));
        let now = history::now_us();
        db.execute_batch(&format!(
            "INSERT INTO moz_places(id, url, title, last_visit_date) VALUES
                (10, 'https://bar.test/', 'Bar', {now}), (11, 'https://menu.test/', 'Menu', {old});
             INSERT INTO moz_bookmarks(id, type, fk, parent, position, title) VALUES
                (20, 1, 10, 3, 0, 'Bar'), (21, 1, 11, 2, 0, 'Menu');
             INSERT INTO moz_historyvisits(place_id, visit_date, visit_type) VALUES
                (10, {now}, 1), (10, {yesterday}, 2), (11, {old}, 1);",
            old = now - 3000 * DAY_US,
            yesterday = now - DAY_US,
        ))
        .unwrap();
        Profile {
            browser: "Firefox".into(),
            key: "firefox".into(),
            family: Family::Firefox,
            name: "test-profile".into(),
            dir,
            default: true,
        }
    }

    /// The whole flow on a Firefox fixture: dry run, import, dedup on a
    /// second run, retention, undo. Uses the process-wide stores.
    #[test]
    fn dry_run_import_reimport_and_undo() {
        let _g = bookmarks::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let profile = firefox_profile(root.path());
        let source = Source::Profile(profile.clone());
        let summary = summary(&profile).unwrap();
        assert_eq!((summary.bookmarks, summary.visits), (2, 3));
        let quiet: Progress = Arc::new(|_| {});
        let before_tree = bookmarks::snapshot();
        let (_, visits_before) = history::counts();
        let dry = run(&source, &Options { dry_run: true, days: Some(30), ..Options::default() }, quiet.clone());
        assert_eq!((dry.bookmarks_found, dry.history_visits_found), (2, 2), "{dry:?}");
        assert_eq!(dry.bookmarks.as_ref().unwrap().added, 2);
        assert!(dry.history.is_none());
        assert_eq!(*bookmarks::snapshot(), *before_tree, "a dry run changes nothing");
        assert_eq!(history::counts().1, visits_before);

        let report = run(&source, &Options::default(), quiet.clone());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.bookmarks.as_ref().unwrap().added, 2);
        // The 3000-day-old visit is beyond the default retention (365 days).
        assert_eq!(report.history.as_ref().unwrap().visits_added, 2);
        let folder = "Other bookmarks \u{203a} Imported from Firefox (test-profile)";
        assert!(bookmarks::entries().iter().any(|e| e.folder == format!("{folder} \u{203a} Bookmarks Toolbar") && e.url == "https://bar.test/"));
        assert!(history::search("bar.test", 5).iter().any(|h| h.visit_count >= 2));

        let again = run(&source, &Options::default(), quiet.clone());
        assert_eq!(again.bookmarks.as_ref().unwrap().added, 0);
        assert_eq!(again.history.as_ref().unwrap().visits_added, 0);
        assert!(again.line().contains("0 bookmarks added"));

        let msg = undo_last().unwrap();
        assert!(msg.contains("removed"), "{msg}");
        // The second run added nothing; undo the first run's history too.
        if let Some(last) = history::last_import() {
            history::undo_import(last.id).unwrap();
        }
        let _ = bookmarks::undo_last_import();
        assert!(!bookmarks::entries().iter().any(|e| e.url == "https://bar.test/"));
        assert!(history::search("bar.test", 5).is_empty());
    }

    #[test]
    fn retention_bounds_the_range() {
        let now = 1_800_000_000_000_000;
        let keep = i64::from(crate::flags::get().history_days);
        assert_eq!(history_since(Some(7), now), now - 7 * DAY_US);
        if keep > 0 {
            assert_eq!(history_since(None, now), now - keep * DAY_US);
            assert_eq!(history_since(Some(100_000), now), now - keep * DAY_US);
        }
    }

    #[test]
    fn export_never_replaces_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = export_to(dir.path()).unwrap();
        let b = export_to(dir.path()).unwrap();
        assert_ne!(a, b);
        assert!(std::fs::read_to_string(&a).unwrap().starts_with("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(b.to_string_lossy().ends_with("-2.html"));
    }
}
