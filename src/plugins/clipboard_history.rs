//! Clipboard history (permission `clipboard.history`): what was copied, kept
//! on disk in the profile's data folder (owner-only), with limits.
//!
//! The watcher (`clipboard_watch`, GTK) turns clipboard changes into
//! [`Capture`]s; [`Store::add`] applies the exclusions, de-duplicates, and
//! evicts the oldest unpinned items past the limits. Images are stored as
//! PNG files next to `index.json`; file lists are kept as references (their
//! URIs), never copied.
//!
//! Why it is sensitive: whatever passes through the clipboard (messages,
//! addresses, keys someone copied from a terminal) is recorded. Copies made
//! by the password manager and the authenticator are never recorded (they
//! are marked with [`mark_sensitive`] and carry the
//! `x-kde-passwordManagerHint` target), nor copies that other password
//! managers mark the same way.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const INDEX: &str = "index.json";
const MAX_INDEX_FILE: u64 = 512 * 1024 * 1024;
const MIB: f64 = 1024.0 * 1024.0;

/// Limits from the plugin's settings (keys of the clipboard manager's
/// manifest; missing keys take these defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    pub max_items: usize,
    pub max_total: u64,
    pub max_item: u64,
    /// Unpinned items older than this go; `None` keeps them.
    pub retention: Option<Duration>,
    pub ignore_private: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_items: 200, max_total: 100 * 1024 * 1024, max_item: 10 * 1024 * 1024, retention: Some(Duration::from_secs(30 * 86_400)), ignore_private: true }
    }
}

impl Limits {
    pub fn from_settings(s: &Map<String, Value>) -> Self {
        let d = Limits::default();
        let num = |k: &str| s.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && *x >= 0.0);
        let bytes = |k: &str, def: u64| num(k).map_or(def, |m| (m * MIB) as u64);
        Limits {
            max_items: num("max_items").map_or(d.max_items, |n| (n as usize).clamp(1, 10_000)),
            max_total: bytes("max_total_mib", d.max_total).max(1024),
            max_item: bytes("max_item_mib", d.max_item).max(1024),
            retention: match num("retention_days") {
                Some(n) if n < 1.0 => None,
                Some(n) => Some(Duration::from_secs((n as u64).min(36_500) * 86_400)),
                None => d.retention,
            },
            ignore_private: s.get("ignore_private").and_then(Value::as_bool).unwrap_or(d.ignore_private),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Html,
    Image,
    Files,
}

/// Where the copy happened, as far as the browser can tell: while its window
/// had the focus (`Browser`) or not (`App`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Browser,
    App,
}

/// A copied image, already decoded once by the watcher.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// FNV-1a of the RGBA pixels: the same picture re-encoded (a clipboard
    /// manager taking over the clipboard) is still the same item.
    pub pixels_hash: u64,
}

/// What the watcher read from the clipboard.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capture {
    pub text: Option<String>,
    pub html: Option<String>,
    pub image: Option<CapturedImage>,
    /// URIs (`file:///...`) of copied files.
    pub files: Vec<String>,
    pub source: Option<Source>,
    /// The owner marked it as a secret (`x-kde-passwordManagerHint`).
    pub secret_hint: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub id: u64,
    pub kind: Kind,
    /// Unix milliseconds of the first and the latest copy.
    pub created: u64,
    pub used: u64,
    pub source: Source,
    #[serde(default)]
    pub pinned: bool,
    /// Plain text (for files: their names, one per line).
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Bytes counted against the limits (text + HTML + image file).
    pub size: u64,
    pub hash: u64,
}

/// FNV-1a, 64 bits: stable across builds (the hash is stored on disk).
pub fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in *p {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

// ---- secrets the browser itself copies ----

static SECRETS: Mutex<Vec<(u64, Instant)>> = Mutex::new(Vec::new());
const SECRET_MEMORY: Duration = Duration::from_secs(15 * 60);

/// The browser is putting a secret (a password, a one-time code) on the
/// clipboard: never record it, whenever the watcher sees it.
pub fn mark_sensitive(text: &str) {
    let mut s = SECRETS.lock().unwrap_or_else(PoisonError::into_inner);
    s.retain(|(_, at)| at.elapsed() < SECRET_MEMORY);
    s.push((text_hash(text), Instant::now()));
}

fn is_marked_secret(hash: u64) -> bool {
    SECRETS.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(h, at)| *h == hash && at.elapsed() < SECRET_MEMORY)
}

fn text_hash(text: &str) -> u64 {
    fnv(&[b"t", text.trim_end_matches(['\n', '\r']).as_bytes()])
}

/// Why a capture was not recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skipped {
    Empty,
    Secret,
    TooLarge,
}

pub struct Store {
    dir: PathBuf,
    items: Vec<Item>,
    next_id: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct IndexFile {
    next_id: u64,
    items: Vec<Item>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn file_name(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
    let mut out = Vec::new();
    let b = last.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(x)) => {
                out.push(x);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Store {
    /// Opens (or starts) the history kept in `dir`. A damaged index starts
    /// an empty history rather than failing.
    pub fn open(dir: PathBuf) -> Self {
        let index: IndexFile = crate::storage::read_text_limited(&dir.join(INDEX), MAX_INDEX_FILE)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let mut s = Store { dir, items: index.items, next_id: index.next_id.max(1) };
        // Images whose file went missing are dropped.
        let dir = s.dir.clone();
        s.items.retain(|i| i.kind != Kind::Image || image_path(&dir, i.id).is_file());
        s.next_id = s.next_id.max(s.items.iter().map(|i| i.id + 1).max().unwrap_or(1));
        s
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Newest (most recently copied) first.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn get(&self, id: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.size).sum()
    }

    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_vec(&IndexFile { next_id: self.next_id, items: self.items.clone() }).map_err(|e| e.to_string())?;
        crate::storage::write_private(&self.dir.join(INDEX), &json).map_err(|e| e.to_string())
    }

    /// Records a capture. Returns the item's id (new, or the existing one a
    /// duplicate was folded into).
    pub fn add(&mut self, c: Capture, limits: &Limits, now: u64) -> Result<u64, Skipped> {
        if c.secret_hint {
            return Err(Skipped::Secret);
        }
        let text = c.text.filter(|t| !t.trim().is_empty());
        if let Some(t) = &text
            && is_marked_secret(text_hash(t))
        {
            return Err(Skipped::Secret);
        }
        let source = c.source.unwrap_or(Source::App);
        let (kind, hash, mut item_text, html, image, files) = if !c.files.is_empty() {
            let joined = c.files.join("\n");
            let names = c.files.iter().map(|u| file_name(u)).collect::<Vec<_>>().join("\n");
            (Kind::Files, fnv(&[b"f", joined.as_bytes()]), names, None, None, c.files)
        } else if let Some(img) = c.image {
            (Kind::Image, fnv(&[b"i", &img.pixels_hash.to_le_bytes()]), text.clone().unwrap_or_default(), None, Some(img), Vec::new())
        } else if let Some(mut t) = text.clone() {
            let html = c.html.filter(|h| !h.trim().is_empty());
            // An app that offers only HTML: GTK hands the markup over as text.
            if html.as_deref().is_some_and(|h| h.trim() == t.trim()) {
                t = html_to_text(&t);
            }
            let kind = if html.is_some() { Kind::Html } else { Kind::Text };
            (kind, text_hash(&t), t, html, None, Vec::new())
        } else {
            return Err(Skipped::Empty);
        };
        if kind == Kind::Image {
            item_text.clear();
        }
        let size = item_text.len() as u64 + html.as_ref().map_or(0, |h| h.len() as u64) + image.as_ref().map_or(0, |i| i.png.len() as u64);
        if size > limits.max_item {
            return Err(Skipped::TooLarge);
        }
        // The same content again: it moves to the top (keeping its pin), and
        // an HTML copy of a text item upgrades it.
        if let Some(pos) = self.items.iter().position(|i| i.hash == hash) {
            let mut it = self.items.remove(pos);
            it.used = now;
            if html.is_some() && it.html.is_none() {
                it.size += html.as_ref().map_or(0, |h| h.len() as u64);
                it.html = html;
                it.kind = Kind::Html;
            }
            let id = it.id;
            self.items.insert(0, it);
            let _ = self.save();
            return Ok(id);
        }
        let id = self.next_id;
        self.next_id += 1;
        let (width, height) = image.as_ref().map_or((0, 0), |i| (i.width, i.height));
        if let Some(img) = &image
            && crate::storage::write_private(&image_path(&self.dir, id), &img.png).is_err()
        {
            return Err(Skipped::TooLarge);
        }
        self.items.insert(0, Item { id, kind, created: now, used: now, source, pinned: false, text: item_text, html, files, width, height, size, hash });
        self.enforce(limits, now);
        let _ = self.save();
        Ok(id)
    }

    /// Applies the limits: retention first, then the oldest unpinned items
    /// go until the count and the total size fit. Pinned items are never
    /// evicted (they still count).
    pub fn enforce(&mut self, limits: &Limits, now: u64) {
        let mut gone = Vec::new();
        if let Some(keep) = limits.retention {
            let cutoff = now.saturating_sub(keep.as_millis() as u64);
            self.items.retain(|i| {
                let keep = i.pinned || i.used >= cutoff;
                if !keep {
                    gone.push(i.id);
                }
                keep
            });
        }
        while self.items.len() > limits.max_items || self.total_bytes() > limits.max_total {
            let Some(pos) = self.items.iter().rposition(|i| !i.pinned) else { break };
            gone.push(self.items.remove(pos).id);
        }
        for id in &gone {
            let _ = std::fs::remove_file(image_path(&self.dir, *id));
        }
        if !gone.is_empty() {
            let _ = self.save();
        }
    }

    pub fn delete(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.id != id);
        let _ = std::fs::remove_file(image_path(&self.dir, id));
        let changed = self.items.len() != before;
        if changed {
            let _ = self.save();
        }
        changed
    }

    pub fn pin(&mut self, id: u64, pinned: bool) -> bool {
        let Some(it) = self.items.iter_mut().find(|i| i.id == id) else { return false };
        it.pinned = pinned;
        let _ = self.save();
        true
    }

    /// Deletes everything (but the pinned items when `keep_pinned`).
    pub fn clear(&mut self, keep_pinned: bool) -> usize {
        let ids: Vec<u64> = self.items.iter().filter(|i| !(keep_pinned && i.pinned)).map(|i| i.id).collect();
        for id in &ids {
            self.delete(*id);
        }
        let _ = self.save();
        ids.len()
    }

    /// Marks an item as just used (pasted, copied back): it moves to the top.
    pub fn touch(&mut self, id: u64, now: u64) {
        if let Some(pos) = self.items.iter().position(|i| i.id == id) {
            let mut it = self.items.remove(pos);
            it.used = now;
            self.items.insert(0, it);
            let _ = self.save();
        }
    }

    /// Items whose text, file names or kind match every word of `query`.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Item> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.items
            .iter()
            .filter(|i| {
                let hay = format!("{} {:?}", i.text.to_lowercase(), i.kind).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .take(limit)
            .collect()
    }
}

/// The readable text of an HTML fragment: tags dropped, line breaks for
/// block ends, the common entities decoded.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let Some(gt) = rest[lt..].find('>') else {
            rest = &rest[lt..];
            break;
        };
        let tag = rest[lt + 1..lt + gt].trim_start_matches('/').to_ascii_lowercase();
        let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        let skip_to = |end: &str, r: &str| r.to_ascii_lowercase().find(end).map(|i| i + end.len());
        rest = &rest[lt + gt + 1..];
        if (name == "script" || name == "style") && !rest.is_empty() {
            let end = format!("</{name}>");
            rest = skip_to(&end, rest).map_or("", |i| &rest[i..]);
            continue;
        }
        if matches!(name, "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") && !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
    }
    out.push_str(rest);
    let decoded = out.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&");
    decoded.trim().to_string()
}

fn image_path(dir: &Path, id: u64) -> PathBuf {
    dir.join("img").join(format!("{id}.png"))
}

/// An item as the plugin page sees it in a list (text shortened).
pub fn summary(i: &Item) -> Value {
    let preview: String = i.text.chars().take(400).collect();
    json!({
        "id": i.id, "kind": i.kind, "created": i.created, "used": i.used, "source": i.source,
        "pinned": i.pinned, "preview": preview, "truncated": i.text.chars().count() > 400,
        "size": i.size, "width": i.width, "height": i.height, "files": i.files.len(),
        "image": (i.kind == Kind::Image).then(|| format!("/__host/clip/{}.png", i.id)),
    })
}

/// The whole item.
pub fn detail(i: &Item) -> Value {
    let mut v = summary(i);
    v["text"] = Value::String(i.text.clone());
    v["html"] = i.html.clone().map_or(Value::Null, Value::String);
    v["fileUris"] = json!(i.files);
    v["fileNames"] = json!(i.files.iter().map(|u| file_name(u)).collect::<Vec<_>>());
    v
}

// ---- the process-wide store ----

static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// Runs `f` on the shared store, opening it from the profile on first use.
pub fn with<R>(f: impl FnOnce(&mut Store) -> R) -> Result<R, String> {
    let mut guard = STORE.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        let dir = crate::storage::profile_dir("clipboard-history")?;
        crate::storage::private_dir(&dir).map_err(|e| e.to_string())?;
        crate::storage::private_dir(&dir.join("img")).map_err(|e| e.to_string())?;
        *guard = Some(Store::open(dir));
    }
    match guard.as_mut() {
        Some(s) => Ok(f(s)),
        None => Err("no clipboard history".into()),
    }
}

/// The PNG of image item `id` (served to the plugin's pages).
pub fn image_bytes(id: u64) -> Option<Vec<u8>> {
    let dir = with(|s| s.get(id).filter(|i| i.kind == Kind::Image).map(|_| s.dir().to_path_buf())).ok().flatten()?;
    crate::storage::read_limited(&image_path(&dir, id), 256 * 1024 * 1024).ok()
}

pub fn now() -> u64 {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> Capture {
        Capture { text: Some(t.into()), source: Some(Source::Browser), ..Default::default() }
    }

    fn image(seed: u8, bytes: usize) -> Capture {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.resize(bytes, seed);
        Capture { image: Some(CapturedImage { png, width: 4, height: 3, pixels_hash: u64::from(seed) }), ..Default::default() }
    }

    fn limits() -> Limits {
        Limits { max_items: 3, max_total: 10_000, max_item: 5_000, retention: Some(Duration::from_secs(86_400)), ignore_private: true }
    }

    #[test]
    fn add_dedup_and_order() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path().to_path_buf());
        let a = s.add(text("alpha"), &limits(), 1000).unwrap();
        let b = s.add(text("beta"), &limits(), 2000).unwrap();
        assert_eq!(s.items()[0].id, b);
        // The same text (even with a trailing newline) folds into the old item.
        assert_eq!(s.add(text("alpha\n"), &limits(), 3000).unwrap(), a);
        assert_eq!(s.items().len(), 2);
        assert_eq!((s.items()[0].id, s.items()[0].used, s.items()[0].created), (a, 3000, 1000));
        // HTML of the same text upgrades it.
        let html = Capture { html: Some("<b>alpha</b>".into()), ..text("alpha") };
        assert_eq!(s.add(html, &limits(), 4000).unwrap(), a);
        assert_eq!(s.get(a).unwrap().kind, Kind::Html);
        assert_eq!(s.add(Capture::default(), &limits(), 5000), Err(Skipped::Empty));
        assert_eq!(s.add(text("   "), &limits(), 5000), Err(Skipped::Empty));
        // Re-encoded image, same pixels: one item.
        let i1 = s.add(image(7, 100), &limits(), 6000).unwrap();
        assert_eq!(s.add(image(7, 120), &limits(), 7000).unwrap(), i1);
        assert!(d.path().join("img").join(format!("{i1}.png")).is_file());
    }

    #[test]
    fn limits_evict_the_oldest_unpinned() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path().to_path_buf());
        let first = s.add(text("one"), &limits(), 1).unwrap();
        s.pin(first, true);
        for (n, t) in ["two", "three", "four", "five"].iter().enumerate() {
            s.add(text(t), &limits(), 2 + n as u64).unwrap();
        }
        let texts: Vec<&str> = s.items().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["five", "four", "one"], "count limit, pinned kept");
        // Size: one item over max_item is refused; big ones push others out.
        assert_eq!(s.add(text(&"x".repeat(6000)), &limits(), 10), Err(Skipped::TooLarge));
        let mut big = limits();
        big.max_total = 3000;
        s.add(image(1, 2500), &big, 11).unwrap();
        assert!(s.total_bytes() <= 3000 || s.items().iter().all(|i| i.pinned || i.kind == Kind::Image));
        assert!(s.items().iter().any(|i| i.pinned), "pinned survives size eviction");
        // Retention: unpinned items unused for longer go.
        let mut short = limits();
        short.retention = Some(Duration::from_secs(1));
        s.enforce(&short, 1_000_000);
        assert!(s.items().iter().all(|i| i.pinned));
    }

    #[test]
    fn secrets_and_hints_are_never_recorded() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path().to_path_buf());
        mark_sensitive("hunter2-very-secret");
        assert_eq!(s.add(text("hunter2-very-secret"), &limits(), 1), Err(Skipped::Secret));
        let hinted = Capture { secret_hint: true, ..text("from keepassxc") };
        assert_eq!(s.add(hinted, &limits(), 2), Err(Skipped::Secret));
        assert!(s.items().is_empty());
    }

    #[test]
    fn persistence_files_and_clear() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("h");
        {
            let mut s = Store::open(dir.clone());
            s.add(text("kept"), &limits(), 1).unwrap();
            let files = Capture { files: vec!["file:///home/u/My%20Doc.pdf".into(), "file:///tmp/b.txt".into()], ..Default::default() };
            let f = s.add(files, &limits(), 2).unwrap();
            assert_eq!(s.get(f).unwrap().text, "My Doc.pdf\nb.txt");
            assert_eq!(s.search("doc", 10).len(), 1);
            assert_eq!(s.search("files", 10).len(), 1, "the kind is searchable");
            s.add(image(3, 64), &limits(), 3).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(INDEX)).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "owner only");
        }
        let mut s = Store::open(dir.clone());
        assert_eq!(s.items().len(), 3);
        let img = s.items()[0].id;
        assert_eq!(s.items()[0].kind, Kind::Image);
        // A missing image file drops its item on load.
        std::fs::remove_file(dir.join("img").join(format!("{img}.png"))).unwrap();
        let mut s2 = Store::open(dir.clone());
        assert_eq!(s2.items().len(), 2);
        let kept = s2.items().iter().find(|i| i.text == "kept").unwrap().id;
        s2.pin(kept, true);
        assert_eq!(s2.clear(true), 1);
        assert_eq!(s2.items().len(), 1);
        assert_eq!(s2.clear(false), 1);
        assert!(Store::open(dir).items().is_empty());
        // A damaged index starts empty.
        std::fs::write(s.dir().join(INDEX), "{nope").unwrap();
        assert!(Store::open(s.dir().to_path_buf()).items().is_empty());
        s.delete(12345);
    }

    #[test]
    fn html_only_copies_get_readable_text() {
        assert_eq!(html_to_text("<b>bold</b> &amp; <i>more</i><br>next<script>x()</script><p>para</p>"), "bold & more\nnext\npara");
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path().to_path_buf());
        let c = Capture { text: Some("<b>bold</b> text".into()), html: Some("<b>bold</b> text".into()), ..Default::default() };
        let id = s.add(c, &limits(), 1).unwrap();
        assert_eq!((s.get(id).unwrap().text.as_str(), s.get(id).unwrap().kind), ("bold text", Kind::Html));
    }

    #[test]
    fn limits_from_settings() {
        let m: Map<String, Value> = serde_json::from_value(json!({"max_items": 5, "max_total_mib": 2, "max_item_mib": 0.5, "retention_days": 0, "ignore_private": false})).unwrap();
        let l = Limits::from_settings(&m);
        assert_eq!((l.max_items, l.max_total, l.max_item, l.retention, l.ignore_private), (5, 2 * 1024 * 1024, 512 * 1024, None, false));
        assert_eq!(Limits::from_settings(&Map::new()), Limits::default());
    }
}
