//! `browsecraft://import`: the profiles found on this computer with what
//! they hold, the import options, the running import's progress, the last
//! result with its undo, importing and exporting a bookmarks.html file.
//! Forms post to the page with its one-time token; the import itself runs
//! on a worker thread (`super::start`) and the browser reloads the page as
//! it progresses.

use std::path::PathBuf;

use super::{Options, Source, detect};
use crate::internal::{self, FormToken};

static FORM_TOKEN: FormToken = FormToken::new();

fn field<'a>(pairs: &'a [(String, zeroize::Zeroizing<String>)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Applies a submission of the page. Returns the message to show.
pub fn handle(query: &str, body: Option<&str>) -> Option<String> {
    let pairs = FORM_TOKEN.take(body.unwrap_or(query))?;
    if field(&pairs, "dismiss-hint").is_some() {
        let mut f = crate::flags::get();
        f.import_hint = false;
        return Some(match crate::flags::set(f) {
            Ok(()) => "The new-tab page will not suggest importing again.".into(),
            Err(e) => e,
        });
    }
    let options = |pairs: &[(String, zeroize::Zeroizing<String>)]| Options {
        bookmarks: field(pairs, "bookmarks").is_some(),
        history: field(pairs, "history").is_some(),
        days: field(pairs, "range").and_then(|r| r.parse().ok()).filter(|d| *d > 0),
        merge: field(pairs, "dest") == Some("merge"),
        dry_run: field(pairs, "dry").is_some(),
    };
    let result = match field(&pairs, "action") {
        Some("import") => {
            let wanted = field(&pairs, "profile").unwrap_or_default();
            match detect::detect().into_iter().find(|p| p.dir.to_string_lossy() == wanted) {
                None => Err("Choose a profile to import from.".to_string()),
                Some(p) => {
                    let o = options(&pairs);
                    if !o.bookmarks && !o.history {
                        Err("Choose bookmarks, history or both.".to_string())
                    } else {
                        super::start(Source::Profile(p), o).map(|()| String::new())
                    }
                }
            }
        }
        Some("html") => {
            let path = PathBuf::from(field(&pairs, "file").unwrap_or_default().trim());
            if !path.is_absolute() || !path.is_file() {
                Err("Enter the full path of a bookmarks .html file.".to_string())
            } else {
                let o = Options { bookmarks: true, history: false, ..options(&pairs) };
                super::start(Source::Html(path), o).map(|()| String::new())
            }
        }
        Some("export") => super::downloads_dir()
            .and_then(|dir| super::export_to(&dir))
            .map(|path| format!("Bookmarks exported to {}", path.display())),
        Some("undo") => super::undo_last(),
        _ => return None,
    };
    Some(match result {
        Ok(message) => message,
        Err(e) => e,
    })
    .filter(|m| !m.is_empty())
}

/// The hint on the new-tab page ("Import from Firefox or Chrome"), while
/// there are no bookmarks and it was not dismissed.
pub fn home_hint() -> Option<String> {
    if !crate::flags::get().import_hint || crate::bookmarks::snapshot().link_count() > 0 {
        return None;
    }
    let token = FORM_TOKEN.issue().ok()?;
    Some(format!(
        "<p class=\"hint\" style=\"margin-top: 36px\">New here? <a href=\"browsecraft://import\">Import bookmarks and history \
         from Firefox or Chrome</a> \u{b7} <a href=\"browsecraft://import?token={token}&amp;dismiss-hint=1\">Don\u{2019}t show again</a></p>"
    ))
}

fn date(us: i64) -> String {
    crate::history::date_string(us)
}

fn report_html(r: &super::Report, token: &str) -> String {
    let mut rows = Vec::new();
    let row = |t: &str, d: String| format!("<div class=\"row\"><div class=\"grow\"><div class=\"t\">{t}</div><div class=\"d\">{}</div></div></div>", internal::escape(&d));
    if r.bookmarks_found > 0 || r.bookmarks.is_some() {
        let mut d = format!("{} bookmarks in {} folders found", r.bookmarks_found, r.folders_found);
        if let Some(b) = &r.bookmarks {
            let verb = if r.dry_run { "would be added" } else { "added" };
            d.push_str(&format!(
                "; {} {verb}, {} already there, {} not web addresses, {} over the limit",
                b.added, b.duplicates, b.invalid, b.over_limit
            ));
        }
        if let Some(dest) = &r.destination {
            d.push_str(&format!(" \u{2192} {dest}"));
        }
        rows.push(row("Bookmarks", d));
    }
    if r.history_visits_found > 0 || r.history.is_some() {
        let mut d = format!("{} visits to {} pages found", r.history_visits_found, r.history_pages_found);
        if let Some(since) = &r.history_since {
            d.push_str(&format!(" (since {since})"));
        }
        if let Some(h) = &r.history {
            d.push_str(&format!("; {} visits added, {} already there, {} new pages", h.visits_added, h.visits_skipped, h.places_added));
        } else if r.dry_run {
            d.push_str("; nothing changed (dry run)");
        }
        rows.push(row("History", d));
    }
    for e in &r.errors {
        rows.push(format!("<div class=\"row\"><div class=\"bad\">{}</div></div>", internal::escape(e)));
    }
    if rows.is_empty() {
        rows.push(row("Nothing to import", "The source has no bookmarks or history (in the range chosen).".into()));
    }
    let undo = if r.dry_run || (r.bookmarks.as_ref().is_none_or(|b| b.added == 0) && r.history.as_ref().is_none_or(|h| h.visits_added == 0)) {
        String::new()
    } else {
        format!(
            "<form method=\"post\" action=\"browsecraft://import\"><input type=\"hidden\" name=\"token\" value=\"{token}\">\
             <input type=\"hidden\" name=\"action\" value=\"undo\"><div class=\"note\"><button class=\"btn ghost\" type=\"submit\">Undo last import</button></div></form>"
        )
    };
    format!(
        "<div class=\"card\"><h2>{} {}</h2>{}{undo}</div>",
        if r.dry_run { "Dry run:" } else { "Imported from" },
        internal::escape(&r.source),
        rows.join("")
    )
}

fn checkbox(name: &str, label: &str, on: bool) -> String {
    format!(
        "<label class=\"l\"><input type=\"checkbox\" name=\"{name}\" value=\"1\"{}> {label}</label>",
        if on { " checked" } else { "" }
    )
}

fn dest_select() -> String {
    "<label class=\"l\">Put bookmarks</label><select class=\"field\" name=\"dest\">\
     <option value=\"folder\" selected>In a new folder in Other bookmarks</option>\
     <option value=\"merge\">Their bookmarks bar into mine, the rest into Other bookmarks</option></select>"
        .to_string()
}

/// The page. `query` may carry `file=` (a dropped bookmarks file).
pub fn page_html(query: &str, message: Option<&str>) -> String {
    let token = match FORM_TOKEN.issue() {
        Ok(token) => token,
        Err(error) => return internal::unavailable_page("Import", &error),
    };
    let file = url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == "file").map(|(_, v)| v.into_owned()).unwrap_or_default();
    let mut body = String::from(
        "<h1>Import</h1><p class=\"sub\">Bring your bookmarks and history from Firefox, Chrome, Chromium, Brave, Edge, \
         Vivaldi or Opera. <b>Bookmarks and history only</b>: passwords, cookies and extensions are not imported. \
         The other browser\u{2019}s files are only read (from a copy), so it can stay open.</p>",
    );
    if let Some(message) = message {
        body.push_str(&format!("<p class=\"ok\">{}</p>", internal::escape(message)));
    }
    let (running, progress, report) = {
        let job = super::job();
        (job.running, job.progress.clone(), job.report.clone())
    };
    if running {
        body.push_str(&format!(
            "<div class=\"card\"><h2>Importing</h2><div class=\"row first\"><div class=\"grow\"><div class=\"t\">{}</div>\
             <div class=\"d\">This page updates as the import runs. You can keep browsing.</div></div></div></div>",
            internal::escape(&progress)
        ));
    } else if let Some(r) = &report {
        body.push_str(&report_html(r, &token));
    }

    // Profiles.
    let profiles = detect::detect();
    body.push_str("<div class=\"card\"><h2>From a browser on this computer</h2>");
    if profiles.is_empty() {
        body.push_str("<div class=\"empty\">No Firefox or Chromium-based browser profile was found.</div>");
    } else {
        body.push_str(&format!(
            "<form method=\"post\" action=\"browsecraft://import\"><input type=\"hidden\" name=\"token\" value=\"{token}\">\
             <input type=\"hidden\" name=\"action\" value=\"import\">"
        ));
        let checked = profiles.iter().position(|p| p.default).unwrap_or(0);
        for (i, p) in profiles.iter().enumerate() {
            let detail = match super::summary(p) {
                Ok(s) => {
                    let last = s.last_visit.map(|t| format!(", last visit {}", date(t))).unwrap_or_default();
                    format!("{} bookmarks, {} pages / {} visits in history{last}", s.bookmarks, s.pages, s.visits)
                }
                Err(e) => format!("Could not read it: {e}"),
            };
            body.push_str(&format!(
                "<div class=\"row{}\"><div class=\"grow\"><label class=\"t\"><input type=\"radio\" name=\"profile\" value=\"{}\"{}> {}{}</label>\
                 <div class=\"d\">{}</div><div class=\"d\">{}</div></div></div>",
                if i == 0 { " first" } else { "" },
                internal::escape(&p.dir.to_string_lossy()),
                if i == checked { " checked" } else { "" },
                internal::escape(&p.label()),
                if p.default { " (default)" } else { "" },
                internal::escape(&detail),
                internal::escape(&p.dir.to_string_lossy()),
            ));
        }
        let keep = crate::flags::get().history_days;
        let keep_note = if keep > 0 { format!(" History older than {keep} days is not kept (Settings).") } else { String::new() };
        body.push_str(&format!(
            "<div class=\"note\">{}{}\
             <label class=\"l\">History range</label><select class=\"field\" name=\"range\">\
             <option value=\"0\" selected>Everything</option><option value=\"365\">Last year</option>\
             <option value=\"90\">Last 90 days</option><option value=\"30\">Last 30 days</option></select>\
             {}{}<div class=\"d\">Bookmarks already in the same folder and visits already in your history are skipped.{}</div>\
             <p><button class=\"btn\" type=\"submit\"{}>Import</button></p></div></form>",
            checkbox("bookmarks", "Bookmarks", true),
            checkbox("history", "History", true),
            dest_select(),
            checkbox("dry", "Dry run: only count what would be imported", false),
            internal::escape(&keep_note),
            if running { " disabled" } else { "" },
        ));
    }
    body.push_str("</div>");

    // HTML files.
    body.push_str(&format!(
        "<div class=\"card\"><h2>From a bookmarks file</h2><form method=\"post\" action=\"browsecraft://import\">\
         <input type=\"hidden\" name=\"token\" value=\"{token}\"><input type=\"hidden\" name=\"action\" value=\"html\">\
         <div class=\"d\">Any browser can export its bookmarks as an HTML file (Safari, older browsers, another computer). \
         You can also drop the file on this window.</div>\
         <label class=\"l\">File</label><input class=\"field\" type=\"text\" name=\"file\" value=\"{}\" placeholder=\"/home/you/Downloads/bookmarks.html\">\
         {}{}<p><button class=\"btn\" type=\"submit\">Import file</button></p></form></div>",
        internal::escape(&file),
        dest_select(),
        checkbox("dry", "Dry run", false),
    ));

    // Export.
    let count = crate::bookmarks::snapshot().link_count();
    body.push_str(&format!(
        "<div class=\"card\"><h2>Export</h2><form method=\"post\" action=\"browsecraft://import\">\
         <input type=\"hidden\" name=\"token\" value=\"{token}\"><input type=\"hidden\" name=\"action\" value=\"export\">\
         <div class=\"row first\"><div class=\"grow\"><div class=\"t\">Export bookmarks to HTML</div>\
         <div class=\"d\">Saves your {count} bookmarks, with their folders, to Downloads as bookmarks-&lt;date&gt;.html, \
         which other browsers can import.</div></div><button class=\"btn ghost\" type=\"submit\">Export</button></div></form></div>"
    ));
    body.push_str(
        "<div class=\"card\"><h2>Not imported</h2><div class=\"note\">Passwords, cookies, open tabs and extensions stay in the other \
         browser. Passwords may come later: Firefox encrypts them with NSS and Chrome with the desktop keyring, so they need \
         more than reading a file. Command line: <b>browsecraft import --list</b>.</div></div>",
    );
    internal::page("Import", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_lists_options_and_needs_the_token() {
        let html = page_html("file=%2Ftmp%2Fb%3Cx%3E.html", None);
        assert!(html.contains("Bookmarks and history only"));
        assert!(html.contains("/tmp/b&lt;x&gt;.html"), "the dropped file is prefilled, escaped");
        assert!(html.contains("Export bookmarks to HTML"));
        assert!(html.contains("passwords"));
        // Without the token nothing happens.
        assert_eq!(handle("", Some("action=export")), None);
        assert_eq!(handle("dismiss-hint=1", None), None);
        let start = html.find("name=\"token\" value=\"").unwrap() + 20;
        let token = &html[start..start + 32];
        let msg = handle("", Some(&format!("token={token}&action=html&file=relative.html"))).unwrap();
        assert!(msg.contains("full path"), "{msg}");
        assert!(handle("", Some(&format!("token={token}&action=html&file=x"))).is_none(), "used up");
    }
}
