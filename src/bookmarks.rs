//! Bookmarks: a tree of folders shared by every tab and window.
//!
//! Two roots, like Chrome's: the bookmarks bar (shown under the address
//! bar; its folders open as menus) and "Other bookmarks". Stored as JSON in
//! `bookmarks.json` in the per-user config folder, replaced atomically on
//! every change. A `bookmarks.lock` file serialises changes between
//! processes: the browser and `browsecraft import` both take it, reload the
//! file if someone else changed it, apply their change and save. A running
//! browser also notices outside changes (`reload_if_changed`) and redraws.
//!
//! The flat `bookmarks.txt` of earlier versions is moved into the bar root
//! on first start and kept as `bookmarks.txt.bak`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::internal::{self, FormToken};

const FILE_NAME: &str = "bookmarks.json";
const OLD_FILE_NAME: &str = "bookmarks.txt";
const LOCK_NAME: &str = "bookmarks.lock";
/// The copy saved before each import.
pub const BACKUP_NAME: &str = "bookmarks.json.import-backup";
const MAX_TITLE_CHARS: usize = 200;
/// Most bookmarks (links) kept; adding and importing stop there.
pub const MAX_BOOKMARKS: usize = 50_000;
/// Most folders kept.
pub const MAX_FOLDERS: usize = 10_000;
/// Deepest folder nesting kept.
pub const MAX_DEPTH: usize = 32;
/// The largest bookmarks.json read.
const MAX_FILE: u64 = 32 << 20;
/// The longest bookmarked address.
const MAX_URL_LEN: usize = 8 * 1024;
/// Links listed by a search on the bookmarks page.
const PAGE_SEARCH_LIMIT: usize = 300;

pub const BAR_ID: u64 = 1;
pub const OTHER_ID: u64 = 2;
pub const BAR_TITLE: &str = "Bookmarks bar";
pub const OTHER_TITLE: &str = "Other bookmarks";

/// A bookmark (`url` is set) or a folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// When it was added, seconds since the Unix epoch (0: unknown).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub added: i64,
    /// The import that added it (0: the user did), for "Undo last import".
    #[serde(default, skip_serializing_if = "is_zero_u")]
    pub import: u64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

fn is_zero_u(n: &u64) -> bool {
    *n == 0
}

impl Node {
    pub fn is_folder(&self) -> bool {
        self.url.is_none()
    }

    fn folder(id: u64, title: &str) -> Node {
        Node { id, title: title.into(), url: None, children: Vec::new(), added: now_secs(), import: 0 }
    }

    /// The bookmarks in this folder and its subfolders.
    pub fn link_count(&self) -> usize {
        self.children.iter().map(|c| if c.is_folder() { c.link_count() } else { 1 }).sum()
    }
}

/// The last import, for its undo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportInfo {
    pub id: u64,
    pub source: String,
    pub time: i64,
    pub added: usize,
}

/// The whole store, as saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    pub version: u32,
    pub next_id: u64,
    pub bar: Node,
    pub other: Node,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_import: Option<ImportInfo>,
}

impl Default for Tree {
    fn default() -> Self {
        Tree {
            version: 1,
            next_id: 3,
            bar: Node::folder(BAR_ID, BAR_TITLE),
            other: Node::folder(OTHER_ID, OTHER_TITLE),
            last_import: None,
        }
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A bookmark with the folders it is in.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: u64,
    pub title: String,
    pub url: String,
    /// "Bookmarks bar › Work", the root first.
    pub folder: String,
}

/// Only web pages are bookmarkable.
pub fn bookmarkable(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")) && url.len() <= MAX_URL_LEN
}

/// The URL without a fragment, so `page#top` and `page` are one bookmark.
fn key(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

pub fn clean_title(title: &str, url: &str) -> String {
    let collapsed: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let title = if collapsed.is_empty() && !url.is_empty() { host_of(url) } else { collapsed };
    title.chars().take(MAX_TITLE_CHARS).collect()
}

/// `example.com` for `https://www.example.com/a`.
pub fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()))
        .unwrap_or_else(|| url.to_string())
}

// ---- the tree ----

impl Tree {
    fn roots(&self) -> [&Node; 2] {
        [&self.bar, &self.other]
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Every bookmark, in order, with its folder path.
    pub fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for root in self.roots() {
            collect(root, &root.title, &mut out);
        }
        out
    }

    /// Every folder (the roots too) as (id, path, depth).
    pub fn folders(&self) -> Vec<(u64, String, usize)> {
        fn walk(node: &Node, path: &str, depth: usize, out: &mut Vec<(u64, String, usize)>) {
            out.push((node.id, path.to_string(), depth));
            for c in node.children.iter().filter(|c| c.is_folder()) {
                walk(c, &format!("{path} \u{203a} {}", c.title), depth + 1, out);
            }
        }
        let mut out = Vec::new();
        for root in self.roots() {
            walk(root, &root.title, 0, &mut out);
        }
        out
    }

    pub fn link_count(&self) -> usize {
        self.bar.link_count() + self.other.link_count()
    }

    pub fn folder_count(&self) -> usize {
        fn count(n: &Node) -> usize {
            n.children.iter().filter(|c| c.is_folder()).map(|c| 1 + count(c)).sum()
        }
        count(&self.bar) + count(&self.other)
    }

    pub fn find(&self, id: u64) -> Option<&Node> {
        fn find(node: &Node, id: u64) -> Option<&Node> {
            if node.id == id {
                return Some(node);
            }
            node.children.iter().find_map(|c| find(c, id))
        }
        self.roots().into_iter().find_map(|r| find(r, id))
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut Node> {
        fn find(node: &mut Node, id: u64) -> Option<&mut Node> {
            if node.id == id {
                return Some(node);
            }
            node.children.iter_mut().find_map(|c| find(c, id))
        }
        if let Some(n) = find(&mut self.bar, id) {
            return Some(n);
        }
        find(&mut self.other, id)
    }

    /// The path of folder `id` ("Other bookmarks › Work").
    pub fn path_of(&self, id: u64) -> Option<String> {
        self.folders().into_iter().find(|(f, _, _)| *f == id).map(|(_, p, _)| p)
    }

    /// Takes node `id` (never a root) out of the tree.
    fn take(&mut self, id: u64) -> Option<Node> {
        fn take(node: &mut Node, id: u64) -> Option<Node> {
            if let Some(i) = node.children.iter().position(|c| c.id == id) {
                return Some(node.children.remove(i));
            }
            node.children.iter_mut().find_map(|c| take(c, id))
        }
        if id == BAR_ID || id == OTHER_ID {
            return None;
        }
        take(&mut self.bar, id).or_else(|| take(&mut self.other, id))
    }

    fn contains_url(&self, url: &str) -> bool {
        self.entries().iter().any(|e| key(&e.url) == key(url))
    }

    /// Adds a bookmark to the bar root (Chrome's star does the same).
    /// `false` for a non-web URL, one already bookmarked or a full store.
    pub fn add(&mut self, title: &str, url: &str) -> bool {
        if !bookmarkable(url) || self.link_count() >= MAX_BOOKMARKS || self.contains_url(url) {
            return false;
        }
        let id = self.new_id();
        self.bar.children.push(Node {
            id,
            title: clean_title(title, url),
            url: Some(url.to_string()),
            children: Vec::new(),
            added: now_secs(),
            import: 0,
        });
        true
    }

    /// Removes every bookmark of `url`. Returns whether there was one.
    pub fn remove_url(&mut self, url: &str) -> bool {
        fn prune(node: &mut Node, url: &str) -> bool {
            let before = node.children.len();
            node.children.retain(|c| c.url.as_deref().is_none_or(|u| key(u) != key(url)));
            let mut removed = node.children.len() != before;
            for c in node.children.iter_mut().filter(|c| c.is_folder()) {
                removed |= prune(c, url);
            }
            removed
        }
        let a = prune(&mut self.bar, url);
        let b = prune(&mut self.other, url);
        a || b
    }

    /// Removes a bookmark or a folder (with its contents).
    pub fn remove_id(&mut self, id: u64) -> bool {
        self.take(id).is_some()
    }

    /// A new folder at the end of folder `parent`.
    pub fn create_folder(&mut self, parent: u64, name: &str) -> Result<u64, String> {
        let name = clean_title(name, "");
        if name.is_empty() {
            return Err("Give the folder a name.".into());
        }
        if self.folder_count() >= MAX_FOLDERS {
            return Err(format!("There are already {MAX_FOLDERS} folders."));
        }
        let depth = self.folders().into_iter().find(|(f, _, _)| *f == parent).map(|(_, _, d)| d);
        match depth {
            None => return Err("That folder no longer exists.".into()),
            Some(d) if d + 1 >= MAX_DEPTH => return Err("Folders cannot be nested that deep.".into()),
            Some(_) => {}
        }
        let id = self.new_id();
        let folder = Node::folder(id, &name);
        match self.find_mut(parent) {
            Some(p) if p.is_folder() => {
                p.children.push(folder);
                Ok(id)
            }
            _ => Err("That folder no longer exists.".into()),
        }
    }

    /// Moves node `id` to the end of folder `parent`. Refuses to move a
    /// folder into itself or a folder inside it.
    pub fn move_to(&mut self, id: u64, parent: u64) -> bool {
        let Some(node) = self.find(id) else { return false };
        fn inside(node: &Node, id: u64) -> bool {
            node.id == id || node.children.iter().any(|c| inside(c, id))
        }
        if inside(node, parent) || !self.find(parent).is_some_and(Node::is_folder) {
            return false;
        }
        let Some(node) = self.take(id) else { return false };
        match self.find_mut(parent) {
            Some(p) => {
                p.children.push(node);
                true
            }
            None => false,
        }
    }

    /// Bookmarks whose title, URL or folder contains every word of `query`
    /// (any case), at most `limit`. All of them (to `limit`) when empty.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Entry> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.entries()
            .into_iter()
            .filter(|e| {
                let hay = format!("{}\n{}\n{}", e.title, e.url, e.folder).to_lowercase();
                terms.iter().all(|t| hay.contains(t.as_str()))
            })
            .take(limit)
            .collect()
    }
}

fn collect(node: &Node, path: &str, out: &mut Vec<Entry>) {
    for c in &node.children {
        match &c.url {
            Some(url) => out.push(Entry { id: c.id, title: c.title.clone(), url: url.clone(), folder: path.to_string() }),
            None => collect(c, &format!("{path} \u{203a} {}", c.title), out),
        }
    }
}

// ---- reading and checking a saved tree ----

/// Parses bookmarks.json, dropping what does not belong: non-web links,
/// nesting beyond `MAX_DEPTH`, more than the caps, duplicate ids (given new
/// ones).
pub fn parse(text: &str) -> Result<Tree, String> {
    let mut tree: Tree = serde_json::from_str(text).map_err(|e| format!("bookmarks.json is not valid: {e}"))?;
    tree.bar.id = BAR_ID;
    tree.bar.url = None;
    tree.bar.title = BAR_TITLE.into();
    tree.other.id = OTHER_ID;
    tree.other.url = None;
    tree.other.title = OTHER_TITLE.into();
    let mut seen = HashSet::from([BAR_ID, OTHER_ID]);
    let mut links = 0;
    let mut folders = 0;
    let mut next = tree.next_id.max(3);
    let mut fix = |node: &mut Node| {
        sanitize(node, 1, &mut seen, &mut links, &mut folders, &mut next);
    };
    fix(&mut tree.bar);
    fix(&mut tree.other);
    tree.next_id = next.max(seen.iter().max().copied().unwrap_or(0) + 1);
    Ok(tree)
}

fn sanitize(node: &mut Node, depth: usize, seen: &mut HashSet<u64>, links: &mut usize, folders: &mut usize, next: &mut u64) {
    let children = std::mem::take(&mut node.children);
    for mut c in children {
        let keep = match &c.url {
            Some(url) => {
                *links += 1;
                bookmarkable(url) && *links <= MAX_BOOKMARKS
            }
            None => {
                *folders += 1;
                depth < MAX_DEPTH && *folders <= MAX_FOLDERS
            }
        };
        if !keep {
            continue;
        }
        if c.id < 3 || !seen.insert(c.id) {
            while seen.contains(next) {
                *next += 1;
            }
            c.id = *next;
            seen.insert(c.id);
        }
        let url = c.url.clone().unwrap_or_default();
        c.title = clean_title(&c.title, &url);
        if c.is_folder() {
            sanitize(&mut c, depth + 1, seen, links, folders, next);
        } else {
            c.children.clear();
        }
        node.children.push(c);
    }
}

pub fn serialize(tree: &Tree) -> Result<String, String> {
    serde_json::to_string_pretty(tree).map_err(|e| e.to_string())
}

/// Reads the `url<TAB>title` lines of the old bookmarks.txt into the bar.
fn migrate_text(text: &str) -> Tree {
    let mut tree = Tree::default();
    let mut seen = HashSet::new();
    for line in text.lines() {
        if tree.bar.children.len() == MAX_BOOKMARKS {
            break;
        }
        let Some((url, title)) = line.split_once('\t') else { continue };
        if bookmarkable(url) && seen.insert(key(url).to_string()) {
            let id = tree.new_id();
            tree.bar.children.push(Node {
                id,
                title: clean_title(title, url),
                url: Some(url.to_string()),
                children: Vec::new(),
                added: 0,
                import: 0,
            });
        }
    }
    tree
}

// ---- importing ----

/// A bookmark or folder from another browser.
#[derive(Clone, Debug, PartialEq)]
pub enum ImportItem {
    Link { title: String, url: String, added: i64 },
    Folder { title: String, added: i64, items: Vec<ImportItem> },
}

/// What an importer read: the other browser's bar (toolbar) and the rest
/// (its menu, "other" and mobile folders, as subfolders).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedBookmarks {
    /// What the other browser calls its bar ("Bookmarks Toolbar").
    pub bar_title: String,
    pub bar: Vec<ImportItem>,
    pub other: Vec<ImportItem>,
}

impl ImportedBookmarks {
    /// (bookmarks, folders).
    pub fn counts(&self) -> (usize, usize) {
        fn count(items: &[ImportItem]) -> (usize, usize) {
            items.iter().fold((0, 0), |(l, f), i| match i {
                ImportItem::Link { .. } => (l + 1, f),
                ImportItem::Folder { items, .. } => {
                    let (a, b) = count(items);
                    (l + a, f + 1 + b)
                }
            })
        }
        let (a, b) = count(&self.bar);
        let (c, d) = count(&self.other);
        (a + c, b + d)
    }
}

/// Where imported bookmarks go.
#[derive(Clone, Debug, PartialEq)]
pub enum Destination {
    /// A folder with this name under "Other bookmarks" (merged with one of
    /// the same name), the other browser's bar as a subfolder in it.
    Folder(String),
    /// The other browser's bar into ours, the rest into "Other bookmarks".
    Merge,
}

/// What an import added or skipped.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MergeStats {
    pub added: usize,
    pub folders_added: usize,
    /// Already in the same folder.
    pub duplicates: usize,
    /// Not a web address (`javascript:`, `file:`, `place:`, ...).
    pub invalid: usize,
    /// Beyond `MAX_BOOKMARKS`, `MAX_FOLDERS` or `MAX_DEPTH`.
    pub over_limit: usize,
}

struct Merger {
    next_id: u64,
    import: u64,
    stats: MergeStats,
    links: usize,
    folders: usize,
}

impl Merger {
    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Merges `items` into `folder`; folders created here are at `depth`.
    fn merge(&mut self, folder: &mut Node, items: &[ImportItem], depth: usize) {
        let mut keys: HashSet<String> =
            folder.children.iter().filter_map(|c| c.url.as_deref().map(|u| key(u).to_string())).collect();
        for item in items {
            match item {
                ImportItem::Link { title, url, added } => {
                    let url = crate::net::without_userinfo(url).unwrap_or_else(|| url.trim().to_string());
                    if !bookmarkable(&url) {
                        self.stats.invalid += 1;
                        continue;
                    }
                    if keys.contains(key(&url)) {
                        self.stats.duplicates += 1;
                        continue;
                    }
                    if self.links >= MAX_BOOKMARKS {
                        self.stats.over_limit += 1;
                        continue;
                    }
                    keys.insert(key(&url).to_string());
                    let id = self.new_id();
                    folder.children.push(Node {
                        id,
                        title: clean_title(title, &url),
                        url: Some(url),
                        children: Vec::new(),
                        added: *added,
                        import: self.import,
                    });
                    self.links += 1;
                    self.stats.added += 1;
                }
                ImportItem::Folder { title, added, items } => match self.subfolder(folder, title, *added, depth) {
                    Some(i) => self.merge(&mut folder.children[i], items, depth + 1),
                    None => {
                        let (links, _) = ImportedBookmarks { bar: items.clone(), ..Default::default() }.counts();
                        self.stats.over_limit += links;
                    }
                },
            }
        }
    }

    /// The index in `parent` of the folder named `title`, created (at
    /// `depth`) if needed.
    fn subfolder(&mut self, parent: &mut Node, title: &str, added: i64, depth: usize) -> Option<usize> {
        let title = clean_title(title, "");
        let title = if title.is_empty() { "Untitled folder".to_string() } else { title };
        if let Some(i) = parent.children.iter().position(|c| c.is_folder() && c.title == title) {
            return Some(i);
        }
        if depth >= MAX_DEPTH || self.folders >= MAX_FOLDERS {
            return None;
        }
        let mut node = Node::folder(self.new_id(), &title);
        node.added = added;
        node.import = self.import;
        parent.children.push(node);
        self.folders += 1;
        self.stats.folders_added += 1;
        Some(parent.children.len() - 1)
    }
}

/// Merges `incoming` into `tree` (dedup: the same address in the same
/// folder is skipped; folders of the same name are merged), tagging what
/// it adds with a new import id.
pub fn merge_import(tree: &mut Tree, incoming: &ImportedBookmarks, dest: &Destination, source: &str) -> MergeStats {
    let import = tree.last_import.as_ref().map_or(0, |i| i.id) + 1;
    let mut m = Merger {
        next_id: tree.next_id,
        import,
        stats: MergeStats::default(),
        links: tree.link_count(),
        folders: tree.folder_count(),
    };
    match dest {
        Destination::Merge => {
            m.merge(&mut tree.bar, &incoming.bar, 1);
            m.merge(&mut tree.other, &incoming.other, 1);
        }
        Destination::Folder(name) => {
            if let Some(i) = m.subfolder(&mut tree.other, name, now_secs(), 1) {
                let top = &mut tree.other.children[i];
                if !incoming.bar.is_empty() {
                    let bar_title = if incoming.bar_title.is_empty() { BAR_TITLE } else { &incoming.bar_title };
                    if let Some(j) = m.subfolder(top, bar_title, 0, 2) {
                        m.merge(&mut top.children[j], &incoming.bar, 3);
                    }
                }
                m.merge(top, &incoming.other, 2);
            }
        }
    }
    tree.next_id = m.next_id;
    let stats = m.stats;
    if stats.added + stats.folders_added > 0 {
        tree.last_import = Some(ImportInfo { id: import, source: source.into(), time: now_secs(), added: stats.added });
    }
    stats
}

/// Removes what import `id` added: its bookmarks, and its folders unless
/// something else was put in them since. Returns the bookmarks removed.
pub fn undo_import_in(tree: &mut Tree, id: u64) -> usize {
    fn prune(node: &mut Node, id: u64) -> usize {
        let mut removed = 0;
        for c in node.children.iter_mut().filter(|c| c.is_folder()) {
            removed += prune(c, id);
        }
        let before = node.children.len();
        node.children.retain(|c| c.import != id || (c.is_folder() && !c.children.is_empty()));
        removed += before - node.children.len();
        removed
    }
    let n = prune(&mut tree.bar, id) + prune(&mut tree.other, id);
    if tree.last_import.as_ref().is_some_and(|i| i.id == id) {
        tree.last_import = None;
    }
    n
}

// ---- the shared store ----

struct Store {
    tree: Arc<Tree>,
    /// The bookmarked addresses (without fragments), for the star.
    urls: HashSet<String>,
    /// The file as last read or written: (modified, length).
    stamp: Option<(SystemTime, u64)>,
    generation: u64,
    error: Option<String>,
}

static STORE: LazyLock<RwLock<Store>> = LazyLock::new(|| {
    RwLock::new(Store { tree: Arc::new(Tree::default()), urls: HashSet::new(), stamp: None, generation: 0, error: None })
});
static FORM_TOKEN: FormToken = FormToken::new();

/// Tests that use the process-wide store (here and in `import`) take this.
#[cfg(test)]
pub static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn file_path() -> Result<PathBuf, String> {
    crate::storage::config_file(FILE_NAME)
}

fn stamp_of(path: &std::path::Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn install(tree: Tree, stamp: Option<(SystemTime, u64)>, error: Option<String>) {
    let urls = tree.entries().into_iter().map(|e| key(&e.url).to_string()).collect();
    let mut store = STORE.write().unwrap_or_else(|p| p.into_inner());
    store.tree = Arc::new(tree);
    store.urls = urls;
    store.stamp = stamp;
    store.generation += 1;
    store.error = error;
}

/// Reads bookmarks.json (migrating bookmarks.txt the first time). The
/// caller holds the lock.
fn read_from_disk() -> Result<(Tree, Option<(SystemTime, u64)>), String> {
    let path = file_path()?;
    match crate::storage::read_text_limited(&path, MAX_FILE) {
        Ok(text) => Ok((parse(&text)?, stamp_of(&path))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let old = crate::storage::config_file(OLD_FILE_NAME)?;
            let Ok(text) = crate::storage::read_text_limited(&old, crate::storage::MAX_SETTINGS_FILE) else {
                return Ok((Tree::default(), None));
            };
            let tree = migrate_text(&text);
            write_tree(&tree)?;
            let _ = std::fs::rename(&old, old.with_extension("txt.bak"));
            Ok((tree, stamp_of(&path)))
        }
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}

fn write_tree(tree: &Tree) -> Result<(), String> {
    let path = file_path()?;
    crate::storage::write_private(&path, serialize(tree)?.as_bytes()).map_err(|e| format!("Could not save {}: {e}", path.display()))
}

fn lock() -> Result<crate::storage::FileLock, String> {
    let path = crate::storage::config_file(LOCK_NAME)?;
    crate::storage::lock_file(&path).map_err(|e| format!("Could not lock the bookmarks: {e}"))
}

/// Reads the bookmarks. Call once at start-up.
pub fn load() {
    let _guard = lock();
    match read_from_disk() {
        Ok((tree, stamp)) => install(tree, stamp, None),
        Err(e) => {
            eprintln!("browsecraft: {e}");
            // Keep the file as it is: nothing is saved until it is fixed or
            // a change replaces it (bookmarks.json.broken keeps a copy).
            if let Ok(path) = file_path() {
                let _ = std::fs::copy(&path, path.with_extension("json.broken"));
            }
            install(Tree::default(), None, Some(e));
        }
    }
}

/// Reloads bookmarks.json if another process changed it since it was read
/// or written here. Returns whether it did.
pub fn reload_if_changed() -> bool {
    let Ok(path) = file_path() else { return false };
    let stamp = stamp_of(&path);
    if stamp.is_none() || STORE.read().unwrap_or_else(|p| p.into_inner()).stamp == stamp {
        return false;
    }
    let _guard = lock();
    match read_from_disk() {
        Ok((tree, stamp)) => {
            install(tree, stamp, None);
            true
        }
        Err(_) => false,
    }
}

/// Applies `change` to the current tree (re-read first if another process
/// changed the file) and saves it when `change` returns `Some`.
pub fn update<R>(change: impl FnOnce(&mut Tree) -> Option<R>) -> Result<Option<R>, String> {
    let _guard = lock()?;
    let path = file_path()?;
    let current = {
        let store = STORE.read().unwrap_or_else(|p| p.into_inner());
        if store.stamp == stamp_of(&path) { Some(store.tree.clone()) } else { None }
    };
    let mut tree = match current {
        Some(tree) => Arc::unwrap_or_clone(tree),
        None => read_from_disk()?.0,
    };
    let Some(result) = change(&mut tree) else {
        return Ok(None);
    };
    write_tree(&tree)?;
    install(tree, stamp_of(&path), None);
    Ok(Some(result))
}

/// The current tree (cheap: shared).
pub fn snapshot() -> Arc<Tree> {
    STORE.read().unwrap_or_else(|p| p.into_inner()).tree.clone()
}

/// Changes whenever the bookmarks do.
pub fn generation() -> u64 {
    STORE.read().unwrap_or_else(|p| p.into_inner()).generation
}

pub fn contains(url: &str) -> bool {
    STORE.read().unwrap_or_else(|p| p.into_inner()).urls.contains(key(url))
}

/// Adds a bookmark to the bar. `false` for a non-web URL, a duplicate, a
/// full store or a failed save.
pub fn add(title: &str, url: &str) -> bool {
    matches!(update(|t| t.add(title, url).then_some(())), Ok(Some(())))
}

/// Removes every bookmark of `url`. Returns whether there was one.
pub fn remove(url: &str) -> bool {
    matches!(update(|t| t.remove_url(url).then_some(())), Ok(Some(())))
}

/// Removes a bookmark or folder by id.
pub fn remove_id(id: u64) -> bool {
    matches!(update(|t| t.remove_id(id).then_some(())), Ok(Some(())))
}

/// Adds the page if it is not bookmarked, removes it if it is. Returns
/// `true` when it is bookmarked afterwards.
pub fn toggle(title: &str, url: &str) -> bool {
    if contains(url) {
        remove(url);
        false
    } else {
        add(title, url)
    }
}

/// Every bookmark with its folder path.
#[cfg(test)]
pub fn entries() -> Vec<Entry> {
    snapshot().entries()
}

/// See `Tree::search`.
pub fn search(query: &str, limit: usize) -> Vec<Entry> {
    snapshot().search(query, limit)
}

/// Imports `incoming` (see `merge_import`), after saving a copy of
/// bookmarks.json as `bookmarks.json.import-backup`.
pub fn import(incoming: &ImportedBookmarks, dest: &Destination, source: &str) -> Result<MergeStats, String> {
    let backup = crate::storage::config_file(BACKUP_NAME)?;
    let result = update(|tree| {
        if let Ok(text) = serialize(tree) {
            let _ = crate::storage::write_private(&backup, text.as_bytes());
        }
        let stats = merge_import(tree, incoming, dest, source);
        Some(stats)
    })?;
    result.ok_or_else(|| "nothing imported".into())
}

/// Undoes the last import. Returns the bookmarks removed.
pub fn undo_last_import() -> Result<usize, String> {
    let n = update(|tree| {
        let id = tree.last_import.as_ref()?.id;
        Some(undo_import_in(tree, id))
    })?;
    n.ok_or_else(|| "There is no import to undo.".into())
}

// ---- browsecraft://bookmarks ----

/// The message the page shows after a change.
static MESSAGE: RwLock<Option<String>> = RwLock::new(None);

fn set_message(text: impl Into<String>) {
    *MESSAGE.write().unwrap_or_else(|p| p.into_inner()) = Some(text.into());
}

/// Applies a submission of the bookmarks page: `remove=<url>`, `delete=<id>`,
/// `move=<id>&to=<folder>`, `folder=<parent>&name=<new folder>` (a POST).
/// Only the page's own, freshly rendered links and forms carry the token.
pub fn apply_form(query: &str, body: Option<&str>) -> bool {
    let Some(pairs) = FORM_TOKEN.take(body.unwrap_or(query)) else {
        return false;
    };
    let field = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.to_string());
    let id = |name: &str| field(name).and_then(|v| v.parse::<u64>().ok());
    let mut changed = false;
    if let Some(url) = field("remove") {
        changed |= remove(&url);
    }
    if let Some(n) = id("delete") {
        changed |= remove_id(n);
        set_message("Deleted.");
    }
    if let (Some(n), Some(to)) = (id("move"), id("to")) {
        match update(|t| t.move_to(n, to).then_some(())) {
            Ok(Some(())) => {
                changed = true;
                set_message("Moved.");
            }
            Ok(None) => set_message("It cannot be moved there."),
            Err(e) => set_message(e),
        }
    }
    if let (Some(parent), Some(name)) = (id("parent"), field("name")) {
        match update(|t| Some(t.create_folder(parent, &name))) {
            Ok(Some(Ok(_))) => {
                changed = true;
                set_message(format!("Folder \u{201c}{}\u{201d} created.", clean_title(&name, "")));
            }
            Ok(Some(Err(e))) | Err(e) => set_message(e),
            Ok(None) => {}
        }
    }
    if field("undo-import").is_some() {
        match undo_last_import() {
            Ok(n) => {
                changed = true;
                set_message(format!("Import undone: {n} bookmarks and folders removed."));
            }
            Err(e) => set_message(e),
        }
    }
    changed
}

/// The address the bookmarks page shows (without the token).
pub fn page_url(query: &str) -> String {
    let mut url = String::from("browsecraft://bookmarks");
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        if k == "folder" || k == "q" {
            url.push_str(if url.contains('?') { "&" } else { "?" });
            url.push_str(&format!("{k}={}", internal::encode(&v)));
        }
    }
    url
}

fn letter_of(title: &str) -> String {
    title.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

/// The `browsecraft://bookmarks` page: one folder at a time (`folder=<id>`,
/// the bar by default) with its subfolders and bookmarks, a search
/// (`q=`) across all of them with their folders, and moving
/// (`picking=<id>` lists the folders to move it to). Each call issues a
/// new form token.
pub fn page_html(query: &str) -> String {
    let token = match FORM_TOKEN.issue() {
        Ok(token) => token,
        Err(error) => return internal::unavailable_page("Bookmarks", &error),
    };
    let mut folder_id = BAR_ID;
    let mut q = String::new();
    let mut picking = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "folder" => folder_id = v.parse().unwrap_or(BAR_ID),
            "to" => folder_id = v.parse().unwrap_or(folder_id),
            "q" => q = v.chars().take(200).collect(),
            "picking" => picking = v.parse::<u64>().ok(),
            _ => {}
        }
    }
    let tree = snapshot();
    let folder = match tree.find(folder_id).filter(|n| n.is_folder()) {
        Some(f) => f,
        None => &tree.bar,
    };
    let folder_id = folder.id;
    let message = MESSAGE.write().unwrap_or_else(|p| p.into_inner()).take();
    let error = STORE.read().unwrap_or_else(|p| p.into_inner()).error.clone();
    let mut body = format!(
        "<h1>Bookmarks</h1><p class=\"sub\">{} bookmarks in {} folders. Star a page in the address bar, or press \
         Ctrl + D, to add it to the bookmarks bar.</p>",
        tree.link_count(),
        tree.folder_count()
    );
    if let Some(error) = error {
        body.push_str(&format!("<p class=\"bad\">{}</p>", internal::escape(&error)));
    }
    if let Some(message) = message {
        body.push_str(&format!("<p class=\"ok\">{}</p>", internal::escape(&message)));
    }
    body.push_str(&format!(
        "<form method=\"get\" action=\"browsecraft://bookmarks\"><input class=\"field\" type=\"search\" name=\"q\" \
         value=\"{}\" placeholder=\"Search bookmarks\"></form>",
        internal::escape(&q)
    ));
    let folder_href = |id: u64| format!("browsecraft://bookmarks?folder={id}");
    let link_row = |e: &Entry, first: bool, show_folder: bool| {
        let detail = if show_folder { format!("{} \u{b7} {}", e.folder, e.url) } else { e.url.clone() };
        format!(
            "<div class=\"item{first}\"><div class=\"ico\">{letter}</div>\
             <div class=\"main\"><a class=\"name\" href=\"{url}\">{title}</a><div class=\"url\">{detail}</div></div>\
             <a class=\"btn ghost\" href=\"browsecraft://bookmarks?folder={folder_id}&amp;picking={id}\">Move</a> \
             <a class=\"btn ghost\" href=\"browsecraft://bookmarks?token={token}&amp;folder={folder_id}&amp;delete={id}\">Remove</a></div>",
            first = if first { " first" } else { "" },
            letter = internal::escape(&letter_of(&e.title)),
            url = internal::escape(&e.url),
            title = internal::escape(&e.title),
            detail = internal::escape(&detail),
            id = e.id,
        )
    };

    // Moving: pick the destination folder.
    if let Some(node) = picking.and_then(|id| tree.find(id)) {
        body.push_str(&format!(
            "<div class=\"card\"><h2>Move \u{201c}{}\u{201d} to</h2>",
            internal::escape(&node.title)
        ));
        for (i, (id, path, depth)) in tree.folders().into_iter().enumerate() {
            let inside = node.is_folder() && path_contains(node, id);
            if inside {
                continue;
            }
            body.push_str(&format!(
                "<div class=\"item{}\" style=\"padding-left: {}px\"><div class=\"main\">\
                 <a class=\"name\" href=\"browsecraft://bookmarks?token={token}&amp;move={}&amp;to={id}\">{}</a></div></div>",
                if i == 0 { " first" } else { "" },
                depth * 18,
                node.id,
                internal::escape(path.rsplit(" \u{203a} ").next().unwrap_or(&path)),
            ));
        }
        body.push_str(&format!("<div class=\"note\"><a class=\"btn ghost\" href=\"{}\">Cancel</a></div></div>", folder_href(folder_id)));
        return internal::page("Bookmarks", &body);
    }

    if !q.trim().is_empty() {
        let hits = tree.search(&q, PAGE_SEARCH_LIMIT);
        body.push_str("<div class=\"card\">");
        if hits.is_empty() {
            body.push_str("<div class=\"empty\">No bookmark matches.</div>");
        }
        for (i, e) in hits.iter().enumerate() {
            body.push_str(&link_row(e, i == 0, true));
        }
        body.push_str("</div>");
        return internal::page("Bookmarks", &body);
    }

    // Roots as tabs, then the path to this folder.
    let path = tree.path_of(folder_id).unwrap_or_default();
    body.push_str("<div class=\"note\">");
    for root in tree.roots() {
        let class = if path.starts_with(&root.title) { "btn" } else { "btn ghost" };
        body.push_str(&format!(
            "<a class=\"{class}\" href=\"{}\">{} ({})</a> ",
            folder_href(root.id),
            internal::escape(&root.title),
            root.link_count()
        ));
    }
    body.push_str("<a class=\"btn ghost\" href=\"browsecraft://import\">Import / export…</a></div>");
    body.push_str(&format!("<div class=\"card\"><h2>{}</h2>", internal::escape(&path)));
    let mut first = true;
    if folder_id != BAR_ID && folder_id != OTHER_ID {
        let parent = tree
            .folders()
            .into_iter()
            .rev()
            .find(|(id, _, _)| tree.find(*id).is_some_and(|f| f.children.iter().any(|c| c.id == folder_id)))
            .map_or(BAR_ID, |(id, _, _)| id);
        body.push_str(&format!(
            "<div class=\"item first\"><div class=\"ico\">\u{ab}</div><div class=\"main\"><a class=\"name\" href=\"{}\">Up</a></div>\
             <a class=\"btn ghost\" href=\"browsecraft://bookmarks?folder={folder_id}&amp;picking={folder_id}\">Move folder</a> \
             <a class=\"btn danger\" href=\"browsecraft://bookmarks?token={token}&amp;folder={parent}&amp;delete={folder_id}\">Delete folder</a></div>",
            folder_href(parent)
        ));
        first = false;
    }
    for c in folder.children.iter().filter(|c| c.is_folder()) {
        body.push_str(&format!(
            "<div class=\"item{}\"><div class=\"ico\">\u{bb}</div><div class=\"main\"><a class=\"name\" href=\"{}\">{}</a>\
             <div class=\"url\">Folder, {} bookmarks</div></div></div>",
            if first { " first" } else { "" },
            folder_href(c.id),
            internal::escape(&c.title),
            c.link_count()
        ));
        first = false;
    }
    for c in folder.children.iter() {
        if let Some(url) = &c.url {
            let e = Entry { id: c.id, title: c.title.clone(), url: url.clone(), folder: path.clone() };
            body.push_str(&link_row(&e, first, false));
            first = false;
        }
    }
    if first {
        body.push_str("<div class=\"empty\">This folder is empty.</div>");
    }
    body.push_str("</div>");
    body.push_str(&format!(
        "<div class=\"card\"><h2>New folder here</h2><form method=\"post\" action=\"browsecraft://bookmarks?folder={folder_id}\">\
         <input type=\"hidden\" name=\"token\" value=\"{token}\"><input type=\"hidden\" name=\"parent\" value=\"{folder_id}\">\
         <input class=\"field\" type=\"text\" name=\"name\" placeholder=\"Folder name\">\
         <div class=\"note\"><button class=\"btn\" type=\"submit\">Create folder</button></div></form></div>"
    ));
    if let Some(last) = &tree.last_import {
        body.push_str(&format!(
            "<div class=\"card\"><div class=\"row first\"><div class=\"grow\"><div class=\"t\">Last import</div>\
             <div class=\"d\">{} bookmarks from {}</div></div>\
             <a class=\"btn ghost\" href=\"browsecraft://bookmarks?token={token}&amp;folder={folder_id}&amp;undo-import=1\">Undo</a></div></div>",
            last.added,
            internal::escape(&last.source)
        ));
    }
    internal::page("Bookmarks", &body)
}

/// Whether folder `node` is or contains folder `id`.
fn path_contains(node: &Node, id: u64) -> bool {
    node.id == id || node.children.iter().any(|c| path_contains(c, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::TEST_SERIAL as SERIAL;

    fn reset() {
        install(Tree::default(), None, None);
        if let Ok(path) = file_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn link(title: &str, url: &str) -> ImportItem {
        ImportItem::Link { title: title.into(), url: url.into(), added: 0 }
    }

    fn folder(title: &str, items: Vec<ImportItem>) -> ImportItem {
        ImportItem::Folder { title: title.into(), added: 0, items }
    }

    #[test]
    fn add_remove_toggle_and_dedup() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(add("Example", "https://example.com/"));
        assert!(!add("Again", "https://example.com/#top"), "fragment is the same page");
        assert!(contains("https://example.com/#x"));
        assert_eq!(snapshot().bar.children.len(), 1, "the star adds to the bar");
        assert!(!toggle("Example", "https://example.com/"));
        assert!(!contains("https://example.com/"));
        assert!(toggle("", "https://www.rust-lang.org/learn"));
        assert_eq!(entries()[0].title, "rust-lang.org", "empty titles fall back to the host");
        assert!(remove("https://www.rust-lang.org/learn"));
        assert!(!remove("https://www.rust-lang.org/learn"));
    }

    #[test]
    fn only_web_pages_can_be_bookmarked() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        for url in ["about:home", "browsecraft://flags", "javascript:alert(1)", "file:///etc/passwd"] {
            assert!(!add("x", url), "{url}");
        }
        assert!(entries().is_empty());
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(clean_title("  a\tb\nc  ", "https://x.test/"), "a b c");
        assert_eq!(clean_title(&"x".repeat(500), "https://x.test/").chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn folders_create_move_delete() {
        let mut t = Tree::default();
        let work = t.create_folder(BAR_ID, "Work").unwrap();
        let deep = t.create_folder(work, "Deep").unwrap();
        assert!(t.create_folder(999, "x").is_err());
        assert!(t.create_folder(BAR_ID, "  ").is_err());
        assert!(t.add("A", "https://a.test/"));
        let a = t.bar.children.last().unwrap().id;
        assert!(t.move_to(a, deep));
        assert_eq!(t.entries()[0].folder, "Bookmarks bar \u{203a} Work \u{203a} Deep");
        assert!(!t.move_to(work, deep), "not into itself");
        assert!(!t.move_to(BAR_ID, OTHER_ID), "roots stay");
        assert!(t.move_to(work, OTHER_ID));
        assert_eq!(t.path_of(deep).unwrap(), "Other bookmarks \u{203a} Work \u{203a} Deep");
        assert_eq!(t.search("deep", 10).len(), 1, "the folder path is searched");
        assert_eq!(t.search("A.TEST", 10).len(), 1);
        assert!(t.remove_id(work));
        assert_eq!(t.link_count(), 0, "with its contents");
        assert!(!t.remove_id(BAR_ID));
    }

    #[test]
    fn migrates_bookmarks_txt_into_the_bar() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let old = crate::storage::config_file(OLD_FILE_NAME).unwrap();
        crate::storage::write_private(&old, b"https://a.test/\tA\nnot a bookmark\njavascript:x\tbad\nhttps://a.test/#f\tdup\nhttps://b.test/\tB\n")
            .unwrap();
        load();
        let e = entries();
        assert_eq!(e.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert!(e.iter().all(|e| e.folder == BAR_TITLE));
        assert!(!old.exists());
        assert!(old.with_extension("txt.bak").exists(), "the old file is kept");
        assert!(file_path().unwrap().exists());
        // Next start reads the JSON.
        install(Tree::default(), None, None);
        load();
        assert_eq!(entries().len(), 2);
        let _ = std::fs::remove_file(old.with_extension("txt.bak"));
        reset();
    }

    #[test]
    fn migration_stops_at_the_cap() {
        let text: String = (0..200_000).map(|i| format!("https://b{i}.test/\tt\n")).collect();
        let start = std::time::Instant::now();
        let tree = migrate_text(&text);
        assert_eq!(tree.link_count(), MAX_BOOKMARKS);
        assert!(start.elapsed() < std::time::Duration::from_secs(10), "{:?}", start.elapsed());
    }

    #[test]
    fn parsing_repairs_bad_files() {
        assert!(parse("not json").is_err());
        assert!(parse("{\"version\":1}").is_err());
        let text = r#"{"version":1,"next_id":1,
            "bar":{"id":7,"title":"x","children":[
                {"id":5,"title":"A","url":"https://a.test/"},
                {"id":5,"title":"dup id","url":"https://b.test/"},
                {"id":6,"title":"bad","url":"javascript:alert(1)"},
                {"id":8,"title":"F","children":[{"id":9,"title":"","url":"https://c.test/"}]}]},
            "other":{"id":1,"title":"y"}}"#;
        let t = parse(text).unwrap();
        assert_eq!((t.bar.id, t.bar.title.as_str(), t.other.id), (BAR_ID, BAR_TITLE, OTHER_ID));
        assert_eq!(t.link_count(), 3);
        let ids: HashSet<u64> = t.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 3, "duplicate ids renumbered");
        assert!(t.next_id > *ids.iter().max().unwrap());
        assert_eq!(t.entries()[2].title, "c.test");
        // Nesting beyond MAX_DEPTH is dropped, not a stack overflow.
        let mut deep = String::from(r#"{"id":100,"title":"d","children":[]}"#);
        for i in 0..200 {
            deep = format!(r#"{{"id":{},"title":"d","children":[{deep}]}}"#, 200 + i);
        }
        let text = format!(r#"{{"version":1,"next_id":3,"bar":{{"id":1,"children":[{deep}]}},"other":{{"id":2}}}}"#);
        // serde_json's own recursion limit (128) may refuse it first.
        if let Ok(t) = parse(&text) {
            assert!(t.folders().iter().all(|(_, _, d)| *d < MAX_DEPTH));
        }
        // Round trip.
        let again = parse(&serialize(&t).unwrap()).unwrap();
        assert_eq!(again, t);
    }

    #[test]
    fn import_into_a_folder_and_merge_dedups() {
        let mut t = Tree::default();
        t.add("Mine", "https://mine.test/");
        let incoming = ImportedBookmarks {
            bar_title: "Bookmarks Toolbar".into(),
            bar: vec![link("Mine", "https://mine.test/"), folder("Dev", vec![link("Rust", "https://rust.test/")])],
            other: vec![
                folder("Bookmarks Menu", vec![link("M", "https://menu.test/"), link("M again", "https://menu.test/#x")]),
                link("JS", "javascript:void(0)"),
                link("Place", "place:sort=8"),
            ],
        };
        assert_eq!(incoming.counts(), (6, 2));
        let dest = Destination::Folder("Imported from Firefox (default)".into());
        let s = merge_import(&mut t, &incoming, &dest, "Firefox");
        assert_eq!((s.added, s.folders_added, s.duplicates, s.invalid), (3, 4, 1, 2));
        let paths: Vec<String> = t.entries().iter().map(|e| e.folder.clone()).collect();
        assert!(paths.contains(&"Other bookmarks \u{203a} Imported from Firefox (default) \u{203a} Bookmarks Toolbar \u{203a} Dev".to_string()));
        assert!(paths.contains(&"Other bookmarks \u{203a} Imported from Firefox (default) \u{203a} Bookmarks Menu".to_string()));
        // The same import again adds nothing (folders merge by name, links dedup per folder).
        let again = merge_import(&mut t, &incoming, &dest, "Firefox");
        assert_eq!((again.added, again.folders_added), (0, 0));
        assert_eq!(again.duplicates, 4);
        // Merge mode: the bar into ours; "Mine" is already there.
        let s = merge_import(&mut t, &incoming, &Destination::Merge, "Firefox");
        assert_eq!(s.added, 2);
        assert!(t.bar.children.iter().any(|c| c.title == "Dev" && c.is_folder()));
        assert!(t.other.children.iter().any(|c| c.title == "Bookmarks Menu"));
    }

    #[test]
    fn import_respects_limits() {
        let mut t = Tree::default();
        let many: Vec<ImportItem> = (0..MAX_BOOKMARKS + 10).map(|i| link("x", &format!("https://m{i}.test/"))).collect();
        let s = merge_import(&mut t, &ImportedBookmarks { other: many, ..Default::default() }, &Destination::Merge, "x");
        assert_eq!(s.added, MAX_BOOKMARKS);
        assert_eq!(s.over_limit, 10);
        assert!(!t.add("one more", "https://more.test/"));
        // Too deep: the rest is counted, not added.
        let mut item = link("deep", "https://deep.test/");
        for _ in 0..MAX_DEPTH + 5 {
            item = folder("d", vec![item]);
        }
        let mut t = Tree::default();
        let s = merge_import(&mut t, &ImportedBookmarks { other: vec![item], ..Default::default() }, &Destination::Merge, "x");
        assert_eq!(s.added, 0);
        assert_eq!(s.over_limit, 1);
        assert!(t.folders().iter().all(|(_, _, d)| *d < MAX_DEPTH));
    }

    #[test]
    fn undo_removes_only_the_import() {
        let mut t = Tree::default();
        t.add("Mine", "https://mine.test/");
        let incoming = ImportedBookmarks {
            bar: vec![link("A", "https://a.test/"), folder("F", vec![link("B", "https://b.test/")])],
            other: vec![folder("G", vec![link("C", "https://c.test/")])],
            ..Default::default()
        };
        merge_import(&mut t, &incoming, &Destination::Merge, "Chrome");
        assert_eq!(t.link_count(), 4);
        // The user puts a bookmark of their own into an imported folder.
        let g = t.other.children.iter().find(|c| c.title == "G").unwrap().id;
        t.create_folder(g, "Mine too").unwrap();
        let id = t.last_import.as_ref().unwrap().id;
        undo_import_in(&mut t, id);
        assert_eq!(t.entries().iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["Mine"]);
        assert!(t.other.children.iter().any(|c| c.title == "G"), "kept: it has the user's folder");
        assert!(!t.bar.children.iter().any(|c| c.title == "F"));
        assert!(t.last_import.is_none());
    }

    #[test]
    fn outside_changes_are_noticed() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(add("Here", "https://here.test/"));
        assert!(!reload_if_changed());
        // Another process (browsecraft import) writes the file.
        let mut other = (*snapshot()).clone();
        other.add("There", "https://there.test/");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_tree(&other).unwrap();
        assert!(reload_if_changed());
        assert!(contains("https://there.test/"));
        // A change made here starts from the file, not a stale copy.
        let mut third = other.clone();
        third.add("Third", "https://third.test/");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_tree(&third).unwrap();
        assert!(add("Fourth", "https://fourth.test/"));
        assert!(contains("https://third.test/") && contains("https://fourth.test/"));
        reset();
    }

    #[test]
    fn only_the_rendered_page_can_change() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(add("Keep", "https://keep.test/?a=1&b=2"));
        let id = entries()[0].id;
        assert!(!apply_form(&format!("delete={id}"), None), "no token");
        let html = page_html("");
        let marker = "token=";
        let start = html.find(marker).unwrap() + marker.len();
        let token = html[start..start + 32].to_string();
        assert!(!apply_form(&format!("token={}&delete={id}", "0".repeat(32)), None));
        assert!(contains("https://keep.test/?a=1&b=2"));
        assert!(apply_form(&format!("token={token}&delete={id}"), None));
        assert!(!contains("https://keep.test/?a=1&b=2"));
        assert!(!apply_form(&format!("token={token}&delete={id}"), None), "token used up");
        // Creating a folder is a POST.
        page_html("");
        let html = page_html("");
        let start = html.find("name=\"token\" value=\"").unwrap() + 20;
        let token = html[start..start + 32].to_string();
        assert!(apply_form("folder=1", Some(&format!("token={token}&parent=1&name=Work+stuff"))));
        assert!(snapshot().bar.children.iter().any(|c| c.title == "Work stuff" && c.is_folder()));
        reset();
    }

    #[test]
    fn page_escapes_markup_and_shows_folders() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(add("<script>x</script>", "https://x.test/?q=\"a\""));
        update(|t| t.create_folder(BAR_ID, "Sub<b>").ok()).unwrap();
        let html = page_html("");
        assert!(!html.contains("<script>x"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("Sub&lt;b&gt;"));
        let html = page_html("q=x.test");
        assert!(html.contains("Bookmarks bar \u{b7} https://x.test/"));
        assert_eq!(page_url("token=abc&folder=7&delete=3"), "browsecraft://bookmarks?folder=7");
        reset();
    }
}
