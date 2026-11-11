//! `browsecraft://sessions`: the session presets page (open, save, update,
//! rename, delete, reorder, export and import) and the start-up chooser
//! (`browsecraft://sessions?startup=1`, for "On startup: Ask every time").
//!
//! Every change is a link or a form carrying the page's one-time token. The
//! browser takes the submission before the page loads (`handle`): changes to
//! the presets file are made here, and what needs the windows (opening,
//! saving the current tabs) comes back as a [`PageAction`].

use std::sync::Mutex;

use crate::internal::{self, FormToken};
use crate::presets::{self, OpenTarget};

pub const URL: &str = "browsecraft://sessions";

static FORM_TOKEN: FormToken = FormToken::new();
/// The result of the last action, shown once: `Ok` news or `Err` problem.
static MESSAGE: Mutex<Option<Result<String, String>>> = Mutex::new(None);

/// What the page asks the browser windows to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageAction {
    Open(u64, OpenTarget),
    /// Save this window's tabs (`all`: every window's) as a new preset.
    Save { name: String, all: bool },
    /// Replace the preset's tabs with the current ones.
    Update { id: u64, all: bool },
    /// The chooser's "Previous session".
    RestorePrevious,
}

/// Shows `message` on the page the next time it renders.
pub fn set_message(message: Result<String, String>) {
    *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
}

fn take_message() -> Option<Result<String, String>> {
    MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The query of a `browsecraft://sessions` address (`""` without one), or
/// `None` for another address.
pub fn query_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix(URL)?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix('?')
}

fn param<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn plain_pairs(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// The address the page shows for `query`: only what changes the view
/// (the chooser, a confirmation) is kept, never a token.
pub fn page_url(query: &str) -> String {
    let pairs = plain_pairs(query);
    if param(&pairs, "startup") == Some("1") {
        return format!("{URL}?startup=1");
    }
    match (param(&pairs, "confirm"), param(&pairs, "id")) {
        (Some(c @ ("delete" | "replace" | "update")), Some(id)) if id.parse::<u64>().is_ok() => format!("{URL}?confirm={c}&id={id}"),
        _ => URL.to_string(),
    }
}

/// Whether a request to the page carries a submission (`handle` takes it).
pub fn is_submission(query: &str, body: Option<&str>) -> bool {
    body.is_some_and(|b| b.contains("token=")) || plain_pairs(query).iter().any(|(k, _)| k == "token")
}

/// Takes a submission of the page (query or form body). Changes to the
/// presets are made here; what needs the windows is returned. Without an
/// outstanding token nothing happens.
pub fn handle(query: &str, body: Option<&str>) -> Option<PageAction> {
    let raw = body.filter(|b| b.contains("token=")).unwrap_or(query);
    let Some(pairs) = FORM_TOKEN.take(raw) else {
        set_message(Err("This link has expired or was already used. Choose again.".into()));
        return None;
    };
    let pairs: Vec<(String, String)> = pairs.into_iter().map(|(k, v)| (k, v.to_string())).collect();
    let id = param(&pairs, "id").and_then(|v| v.parse::<u64>().ok());
    let name = |id: u64| presets::get(id).map(|p| p.name).unwrap_or_default();
    let all = param(&pairs, "scope") == Some("all");
    let result: Result<String, String> = match (param(&pairs, "action").unwrap_or(""), id) {
        ("open", Some(id)) => return Some(PageAction::Open(id, OpenTarget::NewWindow)),
        ("open-here", Some(id)) => return Some(PageAction::Open(id, OpenTarget::Here)),
        ("replace", Some(id)) => return Some(PageAction::Open(id, OpenTarget::Replace)),
        ("restore-previous", _) => return Some(PageAction::RestorePrevious),
        ("save", _) => return Some(PageAction::Save { name: param(&pairs, "name").unwrap_or("").to_string(), all }),
        ("update", Some(id)) => return Some(PageAction::Update { id, all }),
        ("rename", Some(id)) => {
            let new = param(&pairs, "name").unwrap_or("");
            presets::modify(|s| s.rename(id, new, presets::now())).map(|()| format!("Renamed to \u{201c}{}\u{201d}", presets::clean_name(new)))
        }
        ("delete", Some(id)) => presets::modify(|s| s.delete(id)).map(|n| format!("Deleted \u{201c}{n}\u{201d}")),
        ("up", Some(id)) => presets::modify(|s| s.shift(id, -1)).map(|()| format!("Moved \u{201c}{}\u{201d} up", name(id))),
        ("down", Some(id)) => presets::modify(|s| s.shift(id, 1)).map(|()| format!("Moved \u{201c}{}\u{201d} down", name(id))),
        ("export", _) => export(),
        ("import", _) => import(param(&pairs, "file").unwrap_or("")),
        _ => return None,
    };
    set_message(result);
    None
}

fn export() -> Result<String, String> {
    let dir = crate::import::downloads_dir()?;
    let path = export_to(&dir, presets::now(), &presets::store().export())?;
    Ok(format!("Presets exported to {}", path.display()))
}

/// Writes an export into `dir` (Downloads, or the home folder) as a new
/// owner-only file. Never `write_private`: that would make the folder
/// itself owner-only.
fn export_to(dir: &std::path::Path, secs: u64, text: &str) -> Result<std::path::PathBuf, String> {
    for n in 1..100 {
        let name = if n == 1 { format!("browsecraft-presets-{secs}.json") } else { format!("browsecraft-presets-{secs}-{n}.json") };
        let path = dir.join(name);
        match crate::storage::create_new(&path, text.as_bytes(), true) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Could not write {}: {e}", path.display())),
        }
    }
    Err("Too many preset exports this second".into())
}

fn import(file: &str) -> Result<String, String> {
    let file = file.trim();
    if file.is_empty() {
        return Err("Type the path of an exported presets file".into());
    }
    let path = std::path::Path::new(file);
    let text = crate::storage::read_text_limited(path, 8 << 20).map_err(|e| format!("Could not read {file}: {e}"))?;
    let n = presets::modify(|s| s.import(&text, presets::now()))?;
    Ok(format!("Imported {n} preset{}", if n == 1 { "" } else { "s" }))
}

/// A date for "Updated …" (UTC, YYYY-MM-DD).
fn day(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The page for `query` (the view part: `startup=1`, `confirm=…&id=…`).
pub fn page_html(query: &str) -> String {
    let token = match FORM_TOKEN.issue() {
        Ok(t) => t,
        Err(e) => return internal::unavailable_page("Session presets", &e),
    };
    let pairs = plain_pairs(query);
    let store = presets::store();
    let act = |action: &str, id: u64| internal::escape(&format!("{URL}?token={token}&action={action}&id={id}"));
    let confirm = |what: &str, id: u64| internal::escape(&format!("{URL}?confirm={what}&id={id}"));
    let mut body = String::from("<h1>Session presets</h1><p class=\"sub\">Named sets of windows and tabs. Tabs reopen from their address and load when first shown.</p>");

    if let Some(message) = take_message() {
        let (class, text) = match message {
            Ok(t) => ("ok", t),
            Err(t) => ("bad", t),
        };
        body.push_str(&format!("<div class=\"card\"><div class=\"row first\"><div class=\"{class}\" role=status>{}</div></div></div>", internal::escape(&text)));
    }
    if store.read_only {
        body.push_str("<div class=\"card\"><div class=\"row first\"><div class=\"bad\">presets.json was written by a newer BrowseCraft or could not be read: presets can be opened but not changed.</div></div></div>");
    }

    // The start-up chooser.
    if param(&pairs, "startup") == Some("1") {
        body.push_str("<div class=\"card\"><h2>Start with</h2>");
        let mut first = true;
        let mut row = |title: &str, detail: &str, href: &str, button: &str| {
            body.push_str(&format!(
                "<div class=\"row{}\"><div class=\"grow\"><div class=\"t\">{}</div><div class=\"d\">{}</div></div><a class=\"btn{}\" href=\"{}\">{}</a></div>",
                if first { " first" } else { "" },
                internal::escape(title),
                internal::escape(detail),
                if first { "" } else { " ghost" },
                href,
                button
            ));
            first = false;
        };
        if previous_session_exists() {
            row("Previous session", "The windows and tabs open when BrowseCraft last closed.", &internal::escape(&format!("{URL}?token={token}&action=restore-previous")), "Restore");
        }
        for p in &store.presets {
            row(&p.name, &p.summary(), &act("replace", p.id), "Open");
        }
        row("Blank page", "Start with the new-tab page.", "about:home", "Open");
        body.push_str("</div>");
    }

    // A confirmation.
    if let (Some(what), Some(p)) = (param(&pairs, "confirm"), param(&pairs, "id").and_then(|v| v.parse().ok()).and_then(|id| store.get(id))) {
        let (question, action, button) = match what {
            "delete" => (format!("Delete \u{201c}{}\u{201d}? This cannot be undone.", p.name), "delete", "Delete"),
            "replace" => (format!("Close this window's tabs and open \u{201c}{}\u{201d} in their place? Closed tabs can be reopened with Ctrl+Shift+T.", p.name), "replace", "Replace tabs"),
            "update" => (format!("Replace the tabs saved in \u{201c}{}\u{201d} with this window's tabs?", p.name), "update", "Update"),
            _ => (String::new(), "", ""),
        };
        if !action.is_empty() {
            let mut extra = String::new();
            if what == "update" && p.windows.len() > 1 {
                extra = format!(
                    " <a class=\"btn ghost\" href=\"{}\">Update with every window</a>",
                    internal::escape(&format!("{URL}?token={token}&action=update&id={}&scope=all", p.id))
                );
            }
            body.push_str(&format!(
                "<div class=\"card\"><div class=\"row first\"><div class=\"grow\"><div class=\"t\">{}</div></div>\
                 <a class=\"btn{}\" href=\"{}\">{button}</a>{extra} <a class=\"btn ghost\" href=\"{URL}\">Cancel</a></div></div>",
                internal::escape(&question),
                if what == "delete" { " danger" } else { "" },
                act(action, p.id),
            ));
        }
    }

    // Save.
    body.push_str(&format!(
        "<div class=\"card\"><h2>Save the current tabs</h2><form method=\"post\" action=\"{URL}\">\
         <input type=\"hidden\" name=\"token\" value=\"{token}\"><input type=\"hidden\" name=\"action\" value=\"save\">\
         <label class=\"l\">Name</label><input class=\"field\" type=\"text\" name=\"name\" maxlength=\"{}\" placeholder=\"Work\">\
         <label class=\"l\"><input type=\"radio\" name=\"scope\" value=\"window\" checked> This window</label>\
         <label class=\"l\"><input type=\"radio\" name=\"scope\" value=\"all\"> All windows</label>\
         <p><button class=\"btn\" type=\"submit\">Save as preset</button></p></form></div>",
        presets::MAX_NAME
    ));

    // The presets.
    body.push_str("<div class=\"card\"><h2>Presets</h2>");
    if store.presets.is_empty() {
        body.push_str("<div class=\"empty\">No presets yet. Save the current tabs above, or from the command palette (Save session as preset\u{2026}).</div>");
    }
    let n = store.presets.len();
    for (i, p) in store.presets.iter().enumerate() {
        let startup = if store.startup_preset == Some(p.id) && crate::flags::get().startup == crate::flags::Startup::Preset { " \u{b7} opens at start-up" } else { "" };
        let up = if i > 0 { format!("<a class=\"btn ghost\" href=\"{}\" title=\"Move up\">\u{2191}</a> ", act("up", p.id)) } else { String::new() };
        let down = if i + 1 < n { format!("<a class=\"btn ghost\" href=\"{}\" title=\"Move down\">\u{2193}</a> ", act("down", p.id)) } else { String::new() };
        body.push_str(&format!(
            "<div class=\"item{}\"><div class=\"main\"><span class=\"t\">{}</span><div class=\"url\">{} \u{b7} updated {}{}</div>\
             <p><a class=\"btn\" href=\"{}\">Open in new window</a> <a class=\"btn ghost\" href=\"{}\">Add to this window</a> \
             <a class=\"btn ghost\" href=\"{}\">Replace this window\u{2026}</a> <a class=\"btn ghost\" href=\"{}\">Update\u{2026}</a> \
             {up}{down}<a class=\"btn ghost\" href=\"{}\">Delete\u{2026}</a></p>\
             <form method=\"post\" action=\"{URL}\"><input type=\"hidden\" name=\"token\" value=\"{token}\">\
             <input type=\"hidden\" name=\"action\" value=\"rename\"><input type=\"hidden\" name=\"id\" value=\"{}\">\
             <input class=\"field\" type=\"text\" name=\"name\" value=\"{}\" maxlength=\"{}\"> <button class=\"btn ghost\" type=\"submit\">Rename</button></form></div></div>",
            if i == 0 { " first" } else { "" },
            internal::escape(&p.name),
            internal::escape(&p.summary()),
            day(p.updated),
            startup,
            act("open", p.id),
            act("open-here", p.id),
            confirm("replace", p.id),
            confirm("update", p.id),
            confirm("delete", p.id),
            p.id,
            internal::escape(&p.name),
            presets::MAX_NAME,
        ));
    }
    body.push_str("</div>");

    // Export and import.
    body.push_str(&format!(
        "<div class=\"card\"><h2>Export and import</h2>\
         <div class=\"row first\"><div class=\"grow\"><div class=\"t\">Export presets</div>\
         <div class=\"d\">Saves every preset to Downloads as browsecraft-presets-&lt;time&gt;.json.</div></div>\
         <a class=\"btn ghost\" href=\"{}\">Export</a></div>\
         <form method=\"post\" action=\"{URL}\"><input type=\"hidden\" name=\"token\" value=\"{token}\">\
         <input type=\"hidden\" name=\"action\" value=\"import\">\
         <label class=\"l\">Import from file</label><input class=\"field\" type=\"text\" name=\"file\" placeholder=\"/home/you/Downloads/browsecraft-presets.json\">\
         <div class=\"note\">Imported presets are added; a name already in use gets a number. Containers that do not exist here open as normal tabs.</div>\
         <p><button class=\"btn ghost\" type=\"submit\">Import</button></p></form></div>",
        internal::escape(&format!("{URL}?token={token}&action=export"))
    ));
    body.push_str("<div class=\"card\"><div class=\"note\">On startup (Settings) can open a preset or ask every time. \
                   Command line: <b>browsecraft --preset NAME</b>. Command palette: type <b>$</b> and a name.</div></div>");
    internal::page("Session presets", &body)
}

fn previous_session_exists() -> bool {
    crate::storage::config_file(presets::PREVIOUS_SESSION_FILE).is_ok_and(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The presets store and the page's token and message are global.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn token_of(html: &str) -> String {
        let at = html.find("token=").unwrap() + "token=".len();
        html[at..at + 32].to_string()
    }

    fn reset() {
        let ids: Vec<u64> = presets::all().iter().map(|p| p.id).collect();
        for id in ids {
            presets::modify(|s| s.delete(id)).unwrap();
        }
        take_message();
    }

    fn add(name: &str) -> u64 {
        let w = presets::PresetWindow {
            tabs: vec![presets::PresetTab { url: "https://a.test/".into(), title: "A".into(), container: None, group: None }],
            ..presets::PresetWindow::default()
        };
        presets::modify(|s| s.add(name, vec![w], 1)).unwrap()
    }

    #[test]
    fn presets_export_leaves_the_folder_alone() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let a = export_to(dir.path(), 7, "{}").unwrap();
        let b = export_to(dir.path(), 7, "{}").unwrap();
        assert_ne!(a, b, "an earlier export is kept");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "{}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o755, "the folder keeps its mode");
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o077, 0, "the file is owner-only");
        }
    }

    #[test]
    fn presets_page_urls() {
        assert_eq!(query_of("browsecraft://sessions"), Some(""));
        assert_eq!(query_of("browsecraft://sessions?startup=1"), Some("startup=1"));
        assert_eq!(query_of("browsecraft://sessionsx"), None);
        assert_eq!(query_of("browsecraft://flags"), None);
        assert_eq!(page_url("token=abc&action=open&id=3"), URL);
        assert_eq!(page_url("startup=1&token=x"), "browsecraft://sessions?startup=1");
        assert_eq!(page_url("confirm=delete&id=4"), "browsecraft://sessions?confirm=delete&id=4");
        assert_eq!(page_url("confirm=evil&id=4"), URL);
        assert!(is_submission("token=a&action=up&id=1", None));
        assert!(is_submission("", Some("token=a&action=save")));
        assert!(!is_submission("confirm=delete&id=1", None));
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(1_762_819_200), "2025-11-11");
    }

    #[test]
    fn presets_page_actions_need_the_token() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let a = add("Alpha");
        let b = add("Beta");
        assert_eq!(handle(&format!("action=delete&id={a}"), None), None);
        assert!(presets::get(a).is_some(), "no token, no change");
        let html = page_html("");
        assert!(html.contains("Alpha") && html.contains("Beta"));
        let t = token_of(&html);
        assert_eq!(handle(&format!("token={t}&action=open&id={a}"), None), Some(PageAction::Open(a, OpenTarget::NewWindow)));
        assert_eq!(handle(&format!("token={t}&action=open&id={a}"), None), None, "used up");
        let t = token_of(&page_html(""));
        assert_eq!(handle(&format!("token={t}&action=down&id={a}"), None), None);
        assert_eq!(presets::all().iter().map(|p| p.id).collect::<Vec<_>>(), [b, a]);
        let t = token_of(&page_html(""));
        assert_eq!(handle("", Some(&format!("token={t}&action=rename&id={a}&name=Gamma"))), None);
        assert_eq!(presets::get(a).unwrap().name, "Gamma");
        assert!(page_html("").contains("Renamed to"));
        let t = token_of(&page_html(""));
        assert_eq!(handle("", Some(&format!("token={t}&action=save&name=New&scope=all"))), Some(PageAction::Save { name: "New".into(), all: true }));
        let t = token_of(&page_html(""));
        assert_eq!(handle(&format!("token={t}&action=replace&id={b}"), None), Some(PageAction::Open(b, OpenTarget::Replace)));
        let html = page_html(&format!("confirm=delete&id={b}"));
        assert!(html.contains("cannot be undone"));
        let t = token_of(&html);
        handle(&format!("token={t}&action=delete&id={b}"), None);
        assert!(presets::get(b).is_none());
        reset();
    }

    #[test]
    fn presets_page_chooser_lists_presets() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let a = add("Lavoro");
        let html = page_html("startup=1");
        assert!(html.contains("Start with") && html.contains("Blank page"));
        assert!(html.contains(&format!("action=replace&amp;id={a}")), "a preset replaces the chooser tab");
        assert!(!page_html("").contains("Start with"));
        reset();
    }
}
