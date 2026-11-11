//! Keyboard shortcuts of plugin commands (`[[commands]] shortcut = ...`).
//!
//! A shortcut is parsed once from the manifest, checked against the keys the
//! browser already uses (only a plugin with `shortcuts.override` may take one
//! of those) and, while the plugin is enabled, put in a process-wide table that
//! both key paths consult: winit (the chrome has the keyboard) and the GTK
//! handler of every web view (a page has it). Two enabled plugins asking for
//! the same keys: the first in the registry's order wins and the other is
//! listed as a conflict in the plugins drawer.

use super::registry::Registry;
use std::sync::{PoisonError, RwLock};

/// One key combination. `key` is lower case: a single character (`"v"`,
/// `"1"`, `","`) or a named key (`"f2"`, `"enter"`, `"delete"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

const NAMED: &[&str] = &[
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "enter", "escape", "tab", "space",
    "delete", "backspace", "insert", "home", "end", "pageup", "pagedown", "up", "down", "left", "right",
];

impl KeyCombo {
    /// `"Ctrl+Shift+S"`, `"Cmd+V"` (Cmd is Ctrl here and the platform key on
    /// macOS), `"Alt+F2"`. Modifiers in any order, case-insensitive.
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("shortcut \"{s}\": {why}");
        if s.len() > 40 {
            return Err(bad("too long"));
        }
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let (key, mods) = match parts.split_last() {
            // "Ctrl++" ends in an empty part after the split.
            Some((k, m)) if k.is_empty() && m.last().is_some_and(|x| x.is_empty()) => ("+".to_string(), &m[..m.len() - 1]),
            Some((k, m)) => (k.to_ascii_lowercase(), m),
            None => return Err(bad("empty")),
        };
        let mut c = KeyCombo { ctrl: false, shift: false, alt: false, key: String::new() };
        for m in mods {
            let flag = match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" | "mod" | "primary" => &mut c.ctrl,
                "shift" => &mut c.shift,
                "alt" | "option" => &mut c.alt,
                _ => return Err(bad("modifiers are Ctrl (or Cmd), Shift and Alt")),
            };
            if *flag {
                return Err(bad("a modifier is repeated"));
            }
            *flag = true;
        }
        let key = match key.as_str() {
            "esc" => "escape".to_string(),
            "return" => "enter".to_string(),
            "del" => "delete".to_string(),
            "pgup" => "pageup".to_string(),
            "pgdown" => "pagedown".to_string(),
            k => k.to_string(),
        };
        let single = key.chars().count() == 1 && key.chars().all(|ch| ch.is_ascii_graphic());
        if !(single || NAMED.contains(&key.as_str())) {
            return Err(bad("unknown key"));
        }
        let function = key.starts_with('f') && key.len() > 1;
        if !(c.ctrl || c.alt || function) {
            // A plain letter would take typing away from every page.
            return Err(bad("needs Ctrl or Alt (function keys may stand alone)"));
        }
        c.key = key;
        Ok(c)
    }

    /// What the user reads: `Ctrl+Shift+S` (`Cmd` on macOS).
    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str(if cfg!(target_os = "macos") { "Cmd+" } else { "Ctrl+" });
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        let key = match self.key.as_str() {
            k if k.chars().count() == 1 => k.to_ascii_uppercase(),
            k if k.starts_with('f') => k.to_ascii_uppercase(),
            "pageup" => "PageUp".into(),
            "pagedown" => "PageDown".into(),
            k => {
                let mut ch = k.chars();
                ch.next().map(|f| f.to_ascii_uppercase().to_string() + ch.as_str()).unwrap_or_default()
            }
        };
        out + &key
    }

    /// Matches a key press. `key` is what the key handlers produce: the
    /// character without Shift applied where they can (`"v"`), or a name.
    pub fn matches(&self, ctrl: bool, shift: bool, alt: bool, key: &str) -> bool {
        self.ctrl == ctrl && self.shift == shift && self.alt == alt && self.key.eq_ignore_ascii_case(key)
    }
}

/// Keys the browser itself uses, with what they do. Kept in step with
/// `browser::shortcut_for` by a test in `browser/plugins_host.rs`.
pub const BROWSER_KEYS: &[(&str, &str)] = &[
    ("Ctrl+T", "new tab"),
    ("Ctrl+N", "new window"),
    ("Ctrl+Shift+T", "reopen closed tab"),
    ("Ctrl+W", "close tab"),
    ("Ctrl+Tab", "next tab"),
    ("Ctrl+Shift+Tab", "previous tab"),
    ("Ctrl+PageUp", "previous tab"),
    ("Ctrl+PageDown", "next tab"),
    ("Ctrl+Shift+N", "temporary container tab"),
    ("Ctrl+F", "find"),
    ("Ctrl+P", "command palette"),
    ("Ctrl+Shift+P", "print"),
    ("Ctrl+Shift+R", "hard reload"),
    ("Ctrl+Shift+J", "JavaScript console"),
    ("Ctrl+,", "settings"),
    ("Ctrl+Shift+Delete", "clear browsing data"),
    ("Ctrl+1", "tab 1"),
    ("Ctrl+2", "tab 2"),
    ("Ctrl+3", "tab 3"),
    ("Ctrl+4", "tab 4"),
    ("Ctrl+5", "tab 5"),
    ("Ctrl+6", "tab 6"),
    ("Ctrl+7", "tab 7"),
    ("Ctrl+8", "tab 8"),
    ("Ctrl+9", "last tab"),
    ("Ctrl+L", "address bar"),
    ("Ctrl+S", "address bar"),
    ("Ctrl+Shift+S", "screenshot"),
    ("Ctrl+R", "reload"),
    ("Ctrl+U", "view source"),
    ("Ctrl+Shift+I", "developer tools"),
    ("Ctrl+H", "history"),
    ("Ctrl+D", "bookmark"),
    ("Ctrl+B", "tab sidebar (after the page: Bold in editors)"),
    ("Ctrl+Shift+B", "bookmarks bar"),
    ("Ctrl+Shift+O", "bookmarks page"),
    ("Ctrl+Q", "quit"),
    ("Ctrl+=", "zoom in"),
    ("Ctrl++", "zoom in"),
    ("Ctrl+-", "zoom out"),
    ("Ctrl+0", "reset zoom"),
    // These keys ignore Shift in the browser.
    ("Ctrl+Shift+D", "bookmark"),
    ("Ctrl+Shift+H", "history"),
    ("Ctrl+Shift+L", "address bar"),
    ("Ctrl+Shift+Q", "quit"),
    ("Ctrl+Shift+U", "view source"),
    ("Ctrl+Shift+W", "close tab"),
    ("Ctrl+Shift+0", "reset zoom"),
    ("Ctrl+Shift+=", "zoom in"),
    ("Ctrl+Shift+-", "zoom out"),
    ("Ctrl+Shift++", "zoom in"),
    ("Ctrl+Left", "back"),
    ("Ctrl+Right", "forward"),
    ("Alt+Left", "back"),
    ("Alt+Right", "forward"),
    ("Alt+Home", "home"),
    ("F1", "command palette"),
    ("F5", "reload"),
    ("F6", "address bar"),
    ("F11", "fullscreen"),
    ("F12", "developer tools"),
];

/// Text editing keys every page and field uses.
pub const EDITING_KEYS: &[(&str, &str)] = &[
    ("Ctrl+C", "copy"),
    ("Ctrl+X", "cut"),
    ("Ctrl+V", "paste"),
    ("Ctrl+Shift+V", "paste as plain text"),
    ("Ctrl+A", "select all"),
    ("Ctrl+Z", "undo"),
    ("Ctrl+Shift+Z", "redo"),
    ("Ctrl+Y", "redo"),
];

/// What `combo` would take away from the browser or from text editing.
pub fn builtin_use(combo: &KeyCombo) -> Option<&'static str> {
    BROWSER_KEYS.iter().chain(EDITING_KEYS).find(|(k, _)| KeyCombo::parse(k).is_ok_and(|b| b == *combo)).map(|(_, what)| *what)
}

/// A plugin command bound to keys, as resolved for the enabled plugins.
#[derive(Clone, Debug, PartialEq)]
pub struct Active {
    pub combo: KeyCombo,
    pub plugin: String,
    pub command: String,
    /// The built-in use it overrides (with `shortcuts.override`).
    pub overrides: Option<&'static str>,
}

/// A shortcut that lost to another plugin's.
#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub combo: KeyCombo,
    pub plugin: String,
    pub command: String,
    pub winner: String,
}

/// The table for the registry's enabled plugins. A command whose `when`
/// setting is off is left out, so its keys keep their usual meaning.
pub fn resolve(reg: &Registry) -> (Vec<Active>, Vec<Conflict>) {
    let mut active: Vec<Active> = Vec::new();
    let mut conflicts = Vec::new();
    for p in reg.enabled() {
        let m = &p.manifest;
        let settings = reg.settings(&m.id);
        for c in &m.commands {
            let Some(combo) = &c.shortcut else { continue };
            if let Some(key) = &c.when
                && settings.get(key).and_then(serde_json::Value::as_bool) != Some(true)
            {
                continue;
            }
            if let Some(w) = active.iter().find(|a| a.combo == *combo) {
                conflicts.push(Conflict { combo: combo.clone(), plugin: m.id.clone(), command: c.id.clone(), winner: w.plugin.clone() });
                continue;
            }
            active.push(Active { combo: combo.clone(), plugin: m.id.clone(), command: c.id.clone(), overrides: builtin_use(combo) });
        }
    }
    (active, conflicts)
}

static TABLE: RwLock<Vec<Active>> = RwLock::new(Vec::new());

/// Replaces the live table (the browser calls this when plugins change).
pub fn install(table: Vec<Active>) {
    *TABLE.write().unwrap_or_else(PoisonError::into_inner) = table;
}

/// The plugin command bound to this key press, if any: `(plugin, command)`.
pub fn lookup(ctrl: bool, shift: bool, alt: bool, key: &str) -> Option<(String, String)> {
    TABLE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|a| a.combo.matches(ctrl, shift, alt, key))
        .map(|a| (a.plugin.clone(), a.command.clone()))
}

/// The live table, for the drawer.
pub fn table() -> Vec<Active> {
    TABLE.read().unwrap_or_else(PoisonError::into_inner).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let c = KeyCombo::parse("Ctrl+Shift+S").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && c.key == "s");
        assert_eq!(KeyCombo::parse("shift+ctrl+s").unwrap(), c, "order and case do not matter");
        assert_eq!(KeyCombo::parse("Cmd+V").unwrap(), KeyCombo::parse("Ctrl+V").unwrap());
        assert_eq!(KeyCombo::parse("Ctrl++").unwrap().key, "+");
        assert_eq!(KeyCombo::parse("Alt+Esc").unwrap().key, "escape");
        assert!(KeyCombo::parse("F2").is_ok(), "function keys may stand alone");
        for bad in ["V", "Shift+V", "Ctrl+Ctrl+V", "Hyper+V", "Ctrl+", "Ctrl+Foo", "", "Ctrl+é"] {
            assert!(KeyCombo::parse(bad).is_err(), "{bad}");
        }
        assert!(c.matches(true, true, false, "S"));
        assert!(!c.matches(true, false, false, "s"));
        if !cfg!(target_os = "macos") {
            assert_eq!(c.label(), "Ctrl+Shift+S");
            assert_eq!(KeyCombo::parse("ctrl+pagedown").unwrap().label(), "Ctrl+PageDown");
        }
    }

    #[test]
    fn builtin_keys_are_known() {
        for (k, _) in BROWSER_KEYS.iter().chain(EDITING_KEYS) {
            assert!(KeyCombo::parse(k).is_ok(), "{k}");
        }
        assert_eq!(builtin_use(&KeyCombo::parse("Ctrl+V").unwrap()), Some("paste"));
        assert_eq!(builtin_use(&KeyCombo::parse("Ctrl+Shift+S").unwrap()), Some("screenshot"));
        assert_eq!(builtin_use(&KeyCombo::parse("Ctrl+Alt+V").unwrap()), None);
    }
}
