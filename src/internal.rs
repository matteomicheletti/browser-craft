//! The shared look of the internal pages (settings, bookmarks, history,
//! passwords, the new-tab page and error pages), plus the small helpers they
//! have in common: HTML escaping and one-time form tokens.
//!
//! The pages are drawn by BrowseCraft's own engine, which does not know the
//! `prefers-color-scheme` media feature or CSS variables, so each page is
//! generated with the current theme's colours (`set_theme` is called whenever
//! the chrome theme changes) and reloaded when it changes.

use std::collections::VecDeque;
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::theme::{self, Theme, ThemeId};

static CURRENT: Mutex<ThemeId> = Mutex::new(ThemeId::LateNight);

/// Records the effective (resolved) theme. Returns `true` when it changed.
pub fn set_theme(theme: &Theme) -> bool {
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    let changed = *current != theme.id;
    *current = theme.id;
    changed
}

/// The theme internal pages are drawn with.
pub fn current() -> &'static Theme {
    theme::get(*CURRENT.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Escapes text for HTML element content and double-quoted attributes.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The `@token@` colours of the internal pages for `theme`.
fn palette(theme: &Theme) -> [(&'static str, u32); 12] {
    let t = &theme.t;
    [
        ("@bg@", t.page),
        ("@surface@", t.card),
        ("@surface2@", t.surface2),
        ("@line@", t.border),
        ("@text@", t.text),
        ("@muted@", t.text_muted),
        ("@accenttext@", t.accent_text),
        ("@accent@", t.accent),
        ("@off@", theme::mix(t.border, t.text_muted, 0.45)),
        ("@dangertext@", t.danger_text),
        ("@danger@", t.danger),
        ("@ok@", t.success),
    ]
}

const BASE_CSS: &str = r#"
body { margin: 0; background: @bg@; color: @text@; font-family: system-ui, "Segoe UI", sans-serif; font-size: 14px; line-height: 1.5 }
main { max-width: 720px; margin: 0 auto; padding: 44px 24px 72px }
h1 { font-size: 28px; font-weight: 700; margin: 0 0 4px }
.sub { color: @muted@; margin: 0 0 28px }
a { color: @accent@ }
.card { background: @surface@; border: 1px solid @line@; border-radius: 14px; margin: 0 0 18px; padding: 4px 20px 6px }
.card h2 { font-size: 12px; color: @muted@; margin: 16px 0 6px; font-weight: 600; text-transform: uppercase; letter-spacing: 1px }
.row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 14px 0; border-top: 1px solid @line@ }
.row.first { border-top: 0 }
.row .grow { flex: 1 }
.t { font-weight: 600; font-size: 14px }
.d { color: @muted@; font-size: 13px; margin-top: 1px }
.seg { display: flex; gap: 2px; background: @bg@; border: 1px solid @line@; border-radius: 10px; padding: 3px }
.seg a { display: block; padding: 6px 14px; border-radius: 7px; color: @muted@; text-decoration: none; font-size: 13px }
.seg a.on { background: @accent@; color: @accenttext@; font-weight: 600 }
.sw { display: flex; width: 46px; height: 26px; border-radius: 13px; padding: 3px; background: @off@; justify-content: flex-start; text-decoration: none }
.sw.on { background: @accent@; justify-content: flex-end }
.sw span { display: block; width: 20px; height: 20px; border-radius: 10px; background: @surface@ }
.btn { display: inline-block; padding: 8px 16px; border: 0; border-radius: 9px; background: @accent@; color: @accenttext@; text-decoration: none; font-size: 13px; font-weight: 600 }
.btn.ghost { background: @surface2@; color: @text@ }
.btn.danger { background: @danger@; color: @dangertext@ }
.item { display: flex; align-items: center; gap: 14px; padding: 12px 0; border-top: 1px solid @line@ }
.item.first { border-top: 0 }
.item .ico { width: 34px; height: 34px; border-radius: 9px; background: @surface2@; color: @accent@; text-align: center; font-weight: 700; line-height: 34px }
.item .main { flex: 1 }
.item .main a.name { color: @text@; text-decoration: none; font-size: 15px; font-weight: 600 }
.item .url { color: @muted@; font-size: 12px; word-break: break-all }
.empty { color: @muted@; text-align: center; padding: 40px 0 36px }
.field { display: block; box-sizing: border-box; width: 100%; padding: 9px 12px; border: 1px solid @line@; border-radius: 9px; background: @bg@; color: @text@; font-size: 14px; margin: 4px 0 12px }
label.l { display: block; color: @muted@; font-size: 12px; font-weight: 600; margin-top: 8px }
.note { color: @muted@; font-size: 13px; padding: 0 0 14px }
.ok { color: @ok@ }
.themes { margin-top: 10px }
.theme { display: inline-block; width: 104px; margin: 0 8px 8px 0; padding: 8px; border: 1px solid @line@; border-radius: 10px; color: @text@; text-decoration: none; font-size: 12px; text-align: center }
.theme.on { border: 2px solid @accent@; padding: 7px; font-weight: 600 }
.sw3 { display: block; height: 30px; margin-bottom: 6px; border: 1px solid; border-radius: 6px; padding: 7px 8px; box-sizing: border-box; text-align: left }
.sw3 i { display: inline-block; width: 14px; height: 14px; border-radius: 7px; margin-right: 4px }
.bad { color: @danger@ }
"#;

/// The shared stylesheet for the current theme.
pub fn css() -> String {
    css_for(current())
}

pub fn css_for(theme: &Theme) -> String {
    themed(BASE_CSS, theme)
}

/// Replaces the `@token@` colours in `text` with the colours of `theme`.
pub fn themed(text: &str, theme: &Theme) -> String {
    let mut out = text.to_string();
    for (token, color) in palette(theme) {
        out = out.replace(token, &theme::css_hex(color));
    }
    out
}

/// A complete internal page: title, the shared stylesheet and `body`.
pub fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><title>{}</title><style>{}</style><main>{}</main>",
        escape(title),
        css(),
        body
    )
}

/// How many rendered pages' tokens a `FormToken` keeps at once.
pub const MAX_FORM_TOKENS: usize = 128;

/// One-time tokens a page's forms and links must send back. Only a page that
/// was rendered can know one, so a link, a pop-up or a back/forward replay of
/// an earlier submission cannot change anything.
pub struct FormToken {
    tokens: Mutex<VecDeque<String>>,
}

impl FormToken {
    pub const fn new() -> Self {
        FormToken {
            tokens: Mutex::new(VecDeque::new()),
        }
    }

    /// Each retained page owns a one-time OS-random token. Rendering another
    /// page preserves older tokens until the bounded store evicts its oldest
    /// entry.
    pub fn issue(&self) -> Result<String, String> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| format!("Could not create a form token: {e}"))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if tokens.len() == MAX_FORM_TOKENS {
            tokens.pop_front();
        }
        tokens.push_back(token.clone());
        Ok(token)
    }

    /// Forgets every outstanding token.
    #[cfg(test)]
    pub fn clear(&self) {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Parses a submitted query (or form body) and returns its pairs if it
    /// carries an outstanding token, which is used up. `None` otherwise.
    /// Values can be passwords, so they are wiped when dropped.
    pub fn take(&self, query: &str) -> Option<Vec<(String, Zeroizing<String>)>> {
        let pairs: Vec<(String, Zeroizing<String>)> = url::form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), Zeroizing::new(v.into_owned())))
            .collect();
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        let sent = pairs.iter().find(|(k, _)| k == "token").map(|(_, v)| v.as_str())?;
        let index = tokens.iter().position(|token| token == sent)?;
        tokens.remove(index);
        Some(pairs)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.tokens.lock().unwrap().len()
    }
}

/// What an internal page shows when it cannot issue a form token.
pub fn unavailable_page(title: &str, error: &str) -> String {
    page(
        &format!("{title} unavailable"),
        &format!("<h1>{}</h1><p class=\"sub bad\">{}</p>", escape(title), escape(error)),
    )
}

/// Percent-encodes a value for use inside a query string.
pub fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// A settings row with a pill switch. `href` is where the switch leads.
pub fn switch_row(first: bool, title: &str, detail: &str, on: bool, href: &str) -> String {
    format!(
        "<div class=\"row{}\"><div class=\"grow\"><div class=\"t\">{}</div><div class=\"d\">{}</div></div>\
         <a class=\"sw{}\" href=\"{}\"><span></span></a></div>",
        if first { " first" } else { "" },
        escape(title),
        escape(detail),
        if on { " on" } else { "" },
        escape(href)
    )
}

/// A settings row with a segmented control. `options` are
/// `(label, href, selected)`.
pub fn segmented_row(
    first: bool,
    title: &str,
    detail: &str,
    options: &[(&str, String, bool)],
) -> String {
    let mut links = String::new();
    for (label, href, selected) in options {
        links.push_str(&format!(
            "<a{} href=\"{}\">{}</a>",
            if *selected { " class=\"on\"" } else { "" },
            escape(href),
            escape(label)
        ));
    }
    format!(
        "<div class=\"row{}\"><div class=\"grow\"><div class=\"t\">{}</div><div class=\"d\">{}</div></div>\
         <div class=\"seg\">{}</div></div>",
        if first { " first" } else { "" },
        escape(title),
        escape(detail),
        links
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_and_quotes() {
        assert_eq!(escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }

    #[test]
    fn css_replaces_every_token_for_every_theme() {
        for t in theme::THEMES {
            let css = css_for(t);
            assert!(!css.contains('@'), "unreplaced token in {}: {css}", t.name);
            assert!(css.contains(&theme::css_hex(t.t.page)), "{} page background", t.name);
        }
        let home = themed(include_str!("home.html"), &theme::LIGHT);
        assert!(!home.contains('@'), "the new-tab page uses known tokens only");
    }

    #[test]
    fn tokens_are_single_use() {
        let tokens = FormToken::new();
        assert!(tokens.take("token=abc").is_none());
        let t = tokens.issue().unwrap();
        assert!(tokens.take("token=wrong&x=1").is_none());
        assert!(tokens.take("x=1").is_none());
        let pairs = tokens.take(&format!("x=1&token={t}")).unwrap();
        assert!(pairs.iter().any(|(k, v)| k == "x" && v.as_str() == "1"));
        assert!(tokens.take(&format!("token={t}")).is_none(), "used up");
    }

    #[test]
    fn outstanding_tokens_are_bounded() {
        let store = FormToken::new();
        let tokens: Vec<_> = (0..=MAX_FORM_TOKENS).map(|_| store.issue().unwrap()).collect();
        assert_eq!(store.len(), MAX_FORM_TOKENS);
        assert!(store.take(&format!("token={}", tokens[0])).is_none(), "evicted");
        assert!(store.take(&format!("token={}", tokens[1])).is_some());
        assert!(store.take(&format!("token={}", tokens.last().unwrap())).is_some());
        assert_eq!(store.len(), MAX_FORM_TOKENS - 2);
    }

    #[test]
    fn switch_and_segmented_rows_carry_state() {
        let on = switch_row(true, "Bar", "Show it", true, "browsecraft://flags?x=1&y=2");
        assert!(on.contains("class=\"sw on\"") && on.contains("&amp;y=2"));
        assert!(switch_row(false, "Bar", "", false, "x").contains("class=\"sw\""));
        let seg = segmented_row(false, "T", "", &[("A", "a".into(), false), ("B", "b".into(), true)]);
        assert!(seg.contains("<a class=\"on\" href=\"b\">B</a>"));
    }
}
