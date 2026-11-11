//! The plugin registry: built-in components, installed plugins and
//! development folders, their enabled state, settings and private storage.
//!
//! Layout (under the per-user config folder, via `crate::storage`):
//! `plugins.json` (dev folders, disabled ids, settings), `plugin-storage.json`
//! (what plugins keep through `bc.storage`) and `plugins/<id>/` (installed
//! copies). Development folders are referenced in place, never copied.

use super::manifest::{self, LoadedPlugin, Manifest, MANIFEST_FILE};
use super::permissions::{Permission, Requested};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub const MAX_STORAGE_KEY: usize = 128;
pub const MAX_STORAGE_VALUE: usize = 64 * 1024;
pub const MAX_STORAGE_TOTAL: usize = 256 * 1024;
pub const MAX_STORAGE_KEYS: usize = 256;
const MAX_DEV_FOLDERS: usize = 16;
const MAX_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Builtin,
    Installed,
    Dev,
}

pub struct Entry {
    /// Plugin id; for a folder that failed to load, its path.
    pub id: String,
    pub source: Source,
    pub dir: Option<PathBuf>,
    /// Loaded plugin, or the validation errors to show in the manager.
    pub loaded: Result<LoadedPlugin, Vec<String>>,
}

impl Entry {
    pub fn manifest(&self) -> Option<&Manifest> {
        self.loaded.as_ref().ok().map(|l| &l.manifest)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    dev_folders: Vec<PathBuf>,
    disabled: BTreeSet<String>,
    settings: BTreeMap<String, Map<String, Value>>,
}

pub struct Registry {
    root: PathBuf,
    state: State,
    entries: Vec<Entry>,
    storage: BTreeMap<String, Map<String, Value>>,
    nonce: String,
    generation: u64,
    fingerprints: HashMap<PathBuf, u64>,
    /// Bridge call counters per plugin (see `bridge::dispatch`).
    pub(super) rate: HashMap<String, (std::time::Instant, u32)>,
}

fn builtin(id: &str, name: &str, description: &str, perms: &[Permission]) -> Entry {
    let manifest = Manifest {
        id: id.into(),
        name: name.into(),
        version: manifest::Version { major: 1, minor: 0, patch: 0, pre: String::new() },
        api: (super::permissions::API_MAJOR, super::permissions::API_MINOR),
        description: description.into(),
        author: "BrowseCraft".into(),
        homepage: String::new(),
        permissions: perms.iter().map(|p| Requested::Api(*p)).collect(),
        content_scripts: Vec::new(),
        options: Vec::new(),
        wasm: None,
    };
    Entry {
        id: id.into(),
        source: Source::Builtin,
        dir: None,
        loaded: Ok(LoadedPlugin { manifest, dir: PathBuf::new(), scripts: Vec::new(), wasm: None }),
    }
}

/// The browser's own components. They are implemented elsewhere; the entries
/// only show their permissions and carry the enable flag.
fn builtins() -> Vec<Entry> {
    vec![
        builtin("builtin.containers", "Multi-Account Containers", "Separate cookies and storage per container; domain rules.", &[Permission::TabsRead, Permission::Storage]),
        builtin("builtin.authenticator", "Authenticator", "Two-factor codes (TOTP/HOTP) from an encrypted vault.", &[Permission::Storage, Permission::ClipboardWrite]),
        builtin("builtin.darkmode", "Dark mode", "Dark rendering of web pages.", &[Permission::PageModify]),
    ]
}

fn random_nonce() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        // No randomness: a time-based value still stops blind forgery by pages
        // that cannot see this process' clock.
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
        b = t.to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Registry {
    /// Opens the registry stored under the per-user config folder.
    pub fn load() -> Result<Self, String> {
        Ok(Self::open(crate::storage::config_dir()?))
    }

    /// Opens a registry rooted at `root` (tests use a temporary folder).
    pub fn open(root: PathBuf) -> Self {
        let read = |name: &str| crate::storage::read_text_limited(&root.join(name), crate::storage::MAX_SETTINGS_FILE * 4).ok();
        let state = read("plugins.json").and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let storage = read("plugin-storage.json").and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let mut r = Self { root, state, entries: Vec::new(), storage, nonce: random_nonce(), generation: 0, fingerprints: HashMap::new(), rate: HashMap::new() };
        r.reload();
        r
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Secret that page scripts attach to bridge messages (see `bridge`).
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// Bumped whenever the generated page script would change: the browser
    /// rebuilds its web views' init scripts when it sees a new value.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn dev_folders(&self) -> &[PathBuf] {
        &self.state.dev_folders
    }

    fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.get(id).is_some_and(|e| e.loaded.is_ok()) && !self.state.disabled.contains(id)
    }

    /// Enabled plugins that loaded without errors.
    pub fn enabled(&self) -> impl Iterator<Item = &LoadedPlugin> {
        self.entries.iter().filter(|e| !self.state.disabled.contains(&e.id)).filter_map(|e| e.loaded.as_ref().ok())
    }

    pub fn set_enabled(&mut self, id: &str, on: bool) -> Result<(), String> {
        if self.get(id).is_none() {
            return Err(format!("unknown plugin {id}"));
        }
        let changed = if on { self.state.disabled.remove(id) } else { self.state.disabled.insert(id.into()) };
        if changed {
            self.generation += 1;
            self.save()?;
        }
        Ok(())
    }

    // ---- settings ----

    /// Saved settings over the manifest defaults.
    pub fn settings(&self, id: &str) -> Map<String, Value> {
        let Some(m) = self.get(id).and_then(Entry::manifest) else { return Map::new() };
        let mut out = m.default_settings();
        if let Some(saved) = self.state.settings.get(id) {
            for o in &m.options {
                if let Some(v) = saved.get(&o.key).and_then(|v| o.check(v).ok()) {
                    out.insert(o.key.clone(), v);
                }
            }
        }
        out
    }

    pub fn set_setting(&mut self, id: &str, key: &str, value: Value) -> Result<(), String> {
        let m = self.get(id).and_then(Entry::manifest).ok_or_else(|| format!("unknown plugin {id}"))?;
        let def = m.options.iter().find(|o| o.key == key).ok_or_else(|| format!("{id} has no setting \"{key}\""))?;
        let v = def.check(&value)?;
        self.state.settings.entry(id.into()).or_default().insert(key.into(), v);
        self.generation += 1;
        self.save()
    }

    // ---- loading ----

    /// Rescans installed plugins and development folders.
    pub fn reload(&mut self) {
        let mut entries = builtins();
        let mut seen: BTreeSet<String> = entries.iter().map(|e| e.id.clone()).collect();
        let mut add = |entries: &mut Vec<Entry>, dir: PathBuf, source: Source| {
            let loaded = manifest::load_dir(&dir);
            let id = match &loaded {
                Ok(l) => l.manifest.id.clone(),
                Err(_) => dir.display().to_string(),
            };
            let loaded = match loaded {
                Ok(l) if !seen.insert(l.manifest.id.clone()) => {
                    Err(vec![format!("id {} is already provided by another plugin; this copy is ignored", l.manifest.id)])
                }
                other => other,
            };
            entries.push(Entry { id, source, dir: Some(dir), loaded });
        };
        for folder in &self.state.dev_folders {
            for dir in scan_folder(folder) {
                add(&mut entries, dir, Source::Dev);
            }
        }
        if let Ok(rd) = std::fs::read_dir(self.plugins_dir()) {
            let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for dir in dirs {
                add(&mut entries, dir, Source::Installed);
            }
        }
        self.entries = entries;
        self.fingerprints = self.state.dev_folders.iter().map(|f| (f.clone(), fingerprint(f))).collect();
        self.generation += 1;
    }

    /// True when a development folder changed on disk and was reloaded. Cheap
    /// enough to call about once a second.
    pub fn poll_changes(&mut self) -> bool {
        let changed = self.state.dev_folders.iter().any(|f| self.fingerprints.get(f) != Some(&fingerprint(f)));
        if changed {
            self.reload();
        }
        changed
    }

    // ---- install / remove ----

    /// Validates a folder without installing it (for the confirmation step).
    pub fn preview(path: &Path) -> Result<Manifest, Vec<String>> {
        manifest::load_dir(path).map(|l| l.manifest)
    }

    /// Copies a plugin folder into the config folder, replacing an earlier
    /// install of the same id (settings and storage are kept).
    pub fn install_from_folder(&mut self, src: &Path) -> Result<String, Vec<String>> {
        let plugin = manifest::load_dir(src)?;
        let id = plugin.manifest.id.clone();
        if self.get(&id).is_some_and(|e| e.source != Source::Installed) {
            return Err(vec![format!("{id} is already provided by a built-in or development plugin")]);
        }
        let plugins = self.plugins_dir();
        crate::storage::private_dir(&plugins).map_err(|e| vec![e.to_string()])?;
        let staging = plugins.join(format!(".installing-{}", &self.nonce[..8]));
        let _ = std::fs::remove_dir_all(&staging);
        let result = (|| -> Result<(), String> {
            let (mut files, mut total) = (0usize, 0u64);
            copy_tree(&plugin.dir, &plugin.dir, &staging, 0, &mut files, &mut total)?;
            // Re-validate the copy: what is loaded later is what was checked.
            manifest::load_dir(&staging).map_err(|e| e.join("; "))?;
            let dest = plugins.join(&id);
            let _ = std::fs::remove_dir_all(&dest);
            std::fs::rename(&staging, &dest).map_err(|e| e.to_string())
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(vec![e]);
        }
        self.reload();
        Ok(id)
    }

    /// Removes an installed plugin (deletes its copy, settings and storage) or
    /// forgets a development plugin's folder. Built-ins cannot be removed.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let entry = self.get(id).ok_or_else(|| format!("unknown plugin {id}"))?;
        match entry.source {
            Source::Builtin => return Err("built-in components cannot be removed; disable them instead".into()),
            Source::Installed => {
                if let Some(dir) = &entry.dir {
                    // Only ever delete inside our own plugins folder.
                    if dir.parent() != Some(self.plugins_dir().as_path()) {
                        return Err("refusing to delete outside the plugins folder".into());
                    }
                    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
                }
            }
            Source::Dev => {
                let dir = entry.dir.clone().unwrap_or_default();
                // Drop the dev folder entry that provides this plugin.
                let folder = self.state.dev_folders.iter().find(|f| scan_folder(f).contains(&dir)).cloned();
                if let Some(f) = folder {
                    self.state.dev_folders.retain(|x| *x != f);
                }
            }
        }
        self.state.settings.remove(id);
        self.state.disabled.remove(id);
        self.storage.remove(id);
        self.save()?;
        self.save_storage()?;
        self.reload();
        Ok(())
    }

    /// Points the registry at a folder that is a plugin or contains plugins.
    pub fn add_dev_folder(&mut self, folder: &Path) -> Result<usize, String> {
        let folder = folder.canonicalize().map_err(|e| format!("{}: {e}", folder.display()))?;
        if !folder.is_dir() {
            return Err(format!("{} is not a folder", folder.display()));
        }
        if self.state.dev_folders.len() >= MAX_DEV_FOLDERS {
            return Err(format!("at most {MAX_DEV_FOLDERS} plugin folders"));
        }
        let n = scan_folder(&folder).len();
        if n == 0 {
            return Err(format!("no {MANIFEST_FILE} in {} or its subfolders", folder.display()));
        }
        if !self.state.dev_folders.contains(&folder) {
            self.state.dev_folders.push(folder);
            self.save()?;
            self.reload();
        }
        Ok(n)
    }

    // ---- persistence ----

    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(&self.state).map_err(|e| e.to_string())?;
        crate::storage::write_private(&self.root.join("plugins.json"), &json).map_err(|e| e.to_string())
    }

    fn save_storage(&self) -> Result<(), String> {
        let json = serde_json::to_vec(&self.storage).map_err(|e| e.to_string())?;
        crate::storage::write_private(&self.root.join("plugin-storage.json"), &json).map_err(|e| e.to_string())
    }

    // ---- bc.storage ----

    pub fn storage_get(&self, id: &str, key: &str) -> Option<&Value> {
        self.storage.get(id)?.get(key)
    }

    pub fn storage_set(&mut self, id: &str, key: &str, value: Value) -> Result<(), String> {
        if key.is_empty() || key.len() > MAX_STORAGE_KEY {
            return Err(format!("storage keys are 1 to {MAX_STORAGE_KEY} bytes"));
        }
        let size = value.to_string().len();
        if size > MAX_STORAGE_VALUE {
            return Err(format!("value is larger than {} KB", MAX_STORAGE_VALUE / 1024));
        }
        let map = self.storage.entry(id.into()).or_default();
        let others: usize = map.iter().filter(|(k, _)| k.as_str() != key).map(|(k, v)| k.len() + v.to_string().len()).sum();
        if others + key.len() + size > MAX_STORAGE_TOTAL {
            return Err(format!("plugin storage is limited to {} KB", MAX_STORAGE_TOTAL / 1024));
        }
        if !map.contains_key(key) && map.len() >= MAX_STORAGE_KEYS {
            return Err(format!("plugin storage is limited to {MAX_STORAGE_KEYS} keys"));
        }
        map.insert(key.into(), value);
        self.save_storage()
    }

    pub fn storage_remove(&mut self, id: &str, key: &str) -> Result<(), String> {
        if let Some(m) = self.storage.get_mut(id)
            && m.remove(key).is_some()
        {
            return self.save_storage();
        }
        Ok(())
    }

    /// Snapshot of a plugin's storage (for its WASM module).
    pub fn storage_all(&self, id: &str) -> Map<String, Value> {
        self.storage.get(id).cloned().unwrap_or_default()
    }

    /// Replaces a plugin's storage after a WASM run, under the same limits.
    pub fn storage_replace(&mut self, id: &str, new: Map<String, Value>) -> Result<(), String> {
        let old = self.storage_all(id);
        if old == new {
            return Ok(());
        }
        self.storage.remove(id);
        for (k, v) in new {
            if let Err(e) = self.storage_set(id, &k, v) {
                self.storage.insert(id.into(), old);
                return Err(e);
            }
        }
        Ok(())
    }
}

/// Plugin folders found at `folder`: itself, or its direct subfolders.
pub fn scan_folder(folder: &Path) -> Vec<PathBuf> {
    if folder.join(MANIFEST_FILE).is_file() {
        return vec![folder.to_path_buf()];
    }
    let Ok(rd) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(MANIFEST_FILE).is_file() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    out.sort();
    out.truncate(64);
    out
}

/// Hash of every file's relative path, size and modification time (bounded).
fn fingerprint(root: &Path) -> u64 {
    fn walk(dir: &Path, depth: usize, h: &mut impl Hasher, n: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut items: Vec<_> = rd.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            if *n > 2000 {
                return;
            }
            *n += 1;
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            // `metadata` follows links: editing a linked file counts too.
            let Ok(meta) = std::fs::metadata(e.path()) else { continue };
            name.hash(h);
            meta.len().hash(h);
            meta.modified().ok().hash(h);
            if meta.is_dir() && depth < MAX_DEPTH {
                walk(&e.path(), depth + 1, h, n);
            }
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut n = 0;
    walk(root, 0, &mut h, &mut n);
    h.finish()
}

/// Copies regular files (and links that stay inside `root`) into `dest`.
fn copy_tree(root: &Path, dir: &Path, dest: &Path, depth: usize, files: &mut usize, total: &mut u64) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("folders are nested too deeply".into());
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue; // .git and friends
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        let target_dest = dest.join(&name);
        if meta.file_type().is_symlink() {
            let real = path.canonicalize().map_err(|_| format!("broken link {}", path.display()))?;
            if !real.starts_with(root) || !real.is_file() {
                return Err(format!("{} is a link that leaves the plugin folder or is not a file", path.display()));
            }
            copy_file(&real, &target_dest, files, total)?;
        } else if meta.is_dir() {
            copy_tree(root, &path, &target_dest, depth + 1, files, total)?;
        } else if meta.is_file() {
            copy_file(&path, &target_dest, files, total)?;
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dest: &Path, files: &mut usize, total: &mut u64) -> Result<(), String> {
    *files += 1;
    if *files > manifest::MAX_FILES {
        return Err(format!("more than {} files", manifest::MAX_FILES));
    }
    let bytes = crate::storage::read_limited(src, manifest::MAX_WASM).map_err(|e| e.to_string())?;
    *total += bytes.len() as u64;
    if *total > manifest::MAX_TOTAL {
        return Err(format!("plugin is larger than {} MB", manifest::MAX_TOTAL >> 20));
    }
    std::fs::write(dest, bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn write_plugin(dir: &Path, id: &str, extra_js: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "id = \"{id}\"\nname = \"T\"\nversion = \"1.0.0\"\napi_version = \"1\"\npermissions = [\"page.modify\", \"storage\"]\n\
                 [[content_scripts]]\nmatches = [\"https://a.test/*\"]\njs = [\"main.js\"]\ncss = [\"s.css\"]\n\
                 [[options]]\nkey = \"on\"\ntype = \"bool\"\ndefault = true\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("main.js"), format!("var x = 1; {extra_js}")).unwrap();
        std::fs::write(dir.join("s.css"), "body{color:red}").unwrap();
    }

    fn reg(root: &Path) -> Registry {
        Registry::open(root.join("cfg"))
    }

    #[test]
    fn builtins_are_listed_removal_refused_but_disable_works() {
        let d = tempfile::tempdir().unwrap();
        let mut r = reg(d.path());
        assert!(r.is_enabled("builtin.darkmode"));
        assert!(r.remove("builtin.darkmode").unwrap_err().contains("cannot be removed"));
        r.set_enabled("builtin.darkmode", false).unwrap();
        assert!(!r.is_enabled("builtin.darkmode"));
        let r2 = reg(d.path());
        assert!(!r2.is_enabled("builtin.darkmode"), "persisted");
        assert!(r2.is_enabled("builtin.authenticator"));
        assert!(!r2.is_enabled("builtin.nope"));
    }

    #[test]
    fn install_copy_toggle_settings_remove() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        write_plugin(&src, "com.t.one", "");
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".git/x"), "x").unwrap();
        let mut r = reg(d.path());
        let id = r.install_from_folder(&src).unwrap();
        assert_eq!(id, "com.t.one");
        let e = r.get(&id).unwrap();
        assert_eq!(e.source, Source::Installed);
        let dest = d.path().join("cfg/plugins/com.t.one");
        assert!(dest.join("main.js").is_file() && !dest.join(".git").exists());
        // Editing the source afterwards does not change the installed copy.
        std::fs::write(src.join("main.js"), "changed").unwrap();
        r.reload();
        assert!(r.enabled().any(|p| p.scripts.first().is_some_and(|s| s.js[0].1.starts_with("var x"))));

        assert_eq!(r.settings(&id)["on"], true);
        r.set_setting(&id, "on", Value::Bool(false)).unwrap();
        assert!(r.set_setting(&id, "on", Value::from("x")).is_err());
        assert!(r.set_setting(&id, "nope", Value::Bool(true)).is_err());
        r.set_enabled(&id, false).unwrap();

        let mut r2 = reg(d.path());
        assert!(!r2.is_enabled(&id));
        assert_eq!(r2.settings(&id)["on"], false);
        r2.remove(&id).unwrap();
        assert!(!dest.exists() && r2.get(&id).is_none());
        assert!(reg(d.path()).get(&id).is_none());
    }

    #[test]
    fn install_rejects_escaping_symlinks_and_bad_manifests() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        write_plugin(&src, "com.t.two", "");
        std::fs::write(d.path().join("outside.txt"), "o").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path().join("outside.txt"), src.join("leak.txt")).unwrap();
            let mut r = reg(d.path());
            let e = r.install_from_folder(&src).unwrap_err();
            assert!(e[0].contains("leaves the plugin folder"), "{e:?}");
            assert!(!d.path().join("cfg/plugins/com.t.two").exists());
            assert!(std::fs::read_dir(d.path().join("cfg/plugins")).unwrap().next().is_none(), "no staging leftovers");
            std::fs::remove_file(src.join("leak.txt")).unwrap();
            assert!(r.install_from_folder(&src).is_ok());
        }
        std::fs::write(src.join("plugin.toml"), "id = \"x\"").unwrap();
        let mut r = reg(d.path());
        assert!(r.install_from_folder(&src).is_err());
    }

    #[test]
    fn dev_folders_load_in_place_and_reload_on_change() {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        write_plugin(&dev.join("a"), "com.dev.a", "");
        write_plugin(&dev.join("b"), "com.dev.b", "");
        std::fs::write(dev.join("b/plugin.toml"), "broken").unwrap();
        let mut r = reg(d.path());
        assert_eq!(r.add_dev_folder(&dev).unwrap(), 2);
        assert!(r.add_dev_folder(&d.path().join("empty-nope")).is_err());
        assert!(r.get("com.dev.a").is_some_and(|e| e.source == Source::Dev));
        let bad = r.entries().iter().find(|e| e.loaded.is_err()).unwrap();
        assert_eq!(bad.source, Source::Dev);
        assert!(!r.poll_changes());
        let g = r.generation();
        std::fs::write(dev.join("a/main.js"), "var changed = 2; // longer").unwrap();
        assert!(r.poll_changes());
        assert!(r.generation() > g);
        assert!(r.enabled().any(|p| p.scripts.first().is_some_and(|s| s.js[0].1.contains("changed"))));
        // A new plugin dropped into the folder appears.
        write_plugin(&dev.join("c"), "com.dev.c", "");
        assert!(r.poll_changes() && r.get("com.dev.c").is_some());
        // Survives a restart; removing forgets the folder, files stay.
        let mut r2 = reg(d.path());
        assert!(r2.get("com.dev.a").is_some());
        r2.remove("com.dev.a").unwrap();
        assert!(r2.get("com.dev.a").is_none() && dev.join("a/plugin.toml").exists());
    }

    #[test]
    fn a_single_plugin_folder_can_be_added_and_duplicate_ids_are_flagged() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("one");
        write_plugin(&src, "com.t.dup", "");
        let mut r = reg(d.path());
        r.install_from_folder(&src).unwrap();
        assert_eq!(r.add_dev_folder(&src).unwrap(), 1);
        let dup: Vec<_> = r.entries().iter().filter(|e| e.dir.is_some()).collect();
        assert_eq!(dup.len(), 2);
        assert!(dup.iter().any(|e| e.loaded.is_err()), "second one is flagged");
        assert_eq!(r.get("com.t.dup").map(|e| e.source), Some(Source::Dev), "dev copy wins");
        assert!(r.install_from_folder(&src).is_err(), "cannot install over a dev plugin");
    }

    #[test]
    fn storage_limits_and_persistence() {
        let d = tempfile::tempdir().unwrap();
        let mut r = reg(d.path());
        r.storage_set("p.a", "k", Value::from("v")).unwrap();
        assert_eq!(r.storage_get("p.a", "k"), Some(&Value::from("v")));
        assert!(r.storage_get("p.b", "k").is_none(), "per plugin");
        assert!(r.storage_set("p.a", "", Value::Null).is_err());
        assert!(r.storage_set("p.a", &"k".repeat(200), Value::Null).is_err());
        assert!(r.storage_set("p.a", "big", Value::from("x".repeat(MAX_STORAGE_VALUE + 1))).is_err());
        for i in 0..4 {
            let _ = r.storage_set("p.a", &format!("f{i}"), Value::from("x".repeat(MAX_STORAGE_VALUE - 10)));
        }
        assert!(r.storage_set("p.a", "over", Value::from("x".repeat(MAX_STORAGE_VALUE - 10))).is_err(), "total cap");
        let r2 = reg(d.path());
        assert_eq!(r2.storage_get("p.a", "k"), Some(&Value::from("v")));
        let mut r3 = reg(d.path());
        r3.storage_remove("p.a", "k").unwrap();
        assert!(reg(d.path()).storage_get("p.a", "k").is_none());
    }
}
