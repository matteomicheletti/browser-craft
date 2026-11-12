//! Address bar suggestions, as in Chrome and Firefox: while an address is
//! typed, the site it most likely starts (from the history) is completed in
//! place, the completion selected so the next key replaces it, and a list of
//! matching history entries and bookmarks opens under the bar. Up and Down
//! walk the list, Enter opens, Escape closes it.

use crate::egui_chrome::Colors;

/// Rows shown at most.
const MAX_ROWS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub url: String,
    pub title: String,
    pub bookmark: bool,
}

/// The address bar's suggestion state.
#[derive(Default)]
pub struct Omnibox {
    /// What the user typed (without the completion).
    pub typed: String,
    pub items: Vec<Suggestion>,
    /// The row picked with Up/Down.
    pub selected: Option<usize>,
    pub open: bool,
    /// Where the list was drawn last frame (a click on it keeps it open).
    pub rect: Option<egui::Rect>,
}

impl Omnibox {
    pub fn close(&mut self) {
        self.open = false;
        self.selected = None;
        self.items.clear();
        self.rect = None;
    }

    /// The user edited the address: new suggestions and, unless the edit
    /// deleted text, the completion to add after `text`.
    pub fn edited(&mut self, text: &str) -> Option<String> {
        let deleting = text.chars().count() <= self.typed.chars().count();
        self.typed = text.to_string();
        self.selected = None;
        if text.trim().is_empty() {
            self.close();
            return None;
        }
        self.items = suggestions(text.trim());
        self.open = !self.items.is_empty();
        if deleting { None } else { autofill(text, &self.items) }
    }

    /// Up (`false`) or Down (`true`): the row now picked, `None` back on
    /// what was typed.
    pub fn step(&mut self, down: bool) -> Option<&Suggestion> {
        let n = self.items.len();
        if n == 0 {
            return None;
        }
        self.selected = match (self.selected, down) {
            (None, true) => Some(0),
            (None, false) => Some(n - 1),
            (Some(i), true) if i + 1 < n => Some(i + 1),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
            (Some(_), true) => None,
        };
        self.selected.and_then(|i| self.items.get(i))
    }
}

/// Bookmarks first among equals, then history by its rank, no address twice.
pub fn suggestions(query: &str) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let mut push = |s: Suggestion, out: &mut Vec<Suggestion>| {
        if !out.iter().any(|o| same_page(&o.url, &s.url)) {
            out.push(s);
        }
    };
    let history = crate::history::search(query, MAX_ROWS * 2);
    let bookmarks = crate::bookmarks::search(query, 3);
    // A bookmark whose site starts with the query beats the history.
    let first = query.split_whitespace().next().unwrap_or("").to_lowercase();
    for b in bookmarks.iter().filter(|b| strip(&b.url).to_lowercase().starts_with(&first)) {
        push(Suggestion { url: b.url.clone(), title: b.title.clone(), bookmark: true }, &mut out);
    }
    for h in history {
        let bookmark = bookmarks.iter().any(|b| same_page(&b.url, &h.url));
        push(Suggestion { url: h.url, title: h.title, bookmark }, &mut out);
    }
    for b in bookmarks {
        push(Suggestion { url: b.url, title: b.title, bookmark: true }, &mut out);
    }
    out.truncate(MAX_ROWS);
    out
}

fn same_page(a: &str, b: &str) -> bool {
    strip(a).trim_end_matches('/') == strip(b).trim_end_matches('/')
}

/// The address without its scheme and `www.`.
fn strip(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.strip_prefix("www.").unwrap_or(rest)
}

/// The text to add after `typed`: the site (`outl` → `ook.office.com/`)
/// while no `/` was typed, the rest of a known address afterwards. Only
/// for ASCII input without spaces (an address, not a search).
pub fn autofill(typed: &str, items: &[Suggestion]) -> Option<String> {
    if typed.is_empty() || !typed.is_ascii() || typed.contains(char::is_whitespace) {
        return None;
    }
    let lower = typed.to_ascii_lowercase();
    for item in items {
        let url = item.url.as_str();
        // Typed with a scheme or `www.`: compare with as much of the address.
        let base = if lower.contains("://") {
            url
        } else if lower.starts_with("www.") {
            url.split_once("://").map_or(url, |(_, r)| r)
        } else {
            strip(url)
        };
        let candidate = if path_typed(&lower) {
            base
        } else {
            // The site only, with its slash.
            let after = base.find("://").map_or(0, |i| i + 3);
            match base[after..].find('/') {
                Some(i) => &base[..=after + i],
                None => base,
            }
        };
        if !candidate.is_ascii() || candidate.len() <= typed.len() {
            continue;
        }
        if candidate.to_ascii_lowercase().starts_with(&lower) {
            return Some(candidate[typed.len()..].to_string());
        }
    }
    None
}

/// Whether the typed text already goes past the site (has a `/` after any
/// scheme).
fn path_typed(lower: &str) -> bool {
    let rest = lower.split_once("://").map_or(lower, |(_, r)| r);
    rest.contains('/')
}

/// The list under the address bar. Returns the address clicked.
pub fn dropdown(ctx: &egui::Context, c: &Colors, omni: &mut Omnibox, under: egui::Rect) -> Option<String> {
    if !omni.open || omni.items.is_empty() {
        omni.rect = None;
        return None;
    }
    let mut clicked = None;
    let width = under.width();
    let area = egui::Area::new(egui::Id::new("browsecraft-omnibox"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(under.min.x, under.max.y + 4.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(c.bar_bg)
                .stroke(egui::Stroke::new(1.0, c.line))
                .corner_radius(egui::CornerRadius::same(10))
                .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: c.shadow })
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(width - 12.0);
                    for (i, item) in omni.items.iter().enumerate() {
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
                        if omni.selected == Some(i) || resp.hovered() {
                            ui.painter().rect_filled(rect, egui::CornerRadius::same(6), c.hover);
                        }
                        let p = ui.painter().with_clip_rect(rect);
                        let glyph = if item.bookmark { crate::icons::STAR } else { crate::icons::GLOBE };
                        let x = rect.min.x + 10.0;
                        p.text(
                            egui::pos2(x, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            glyph,
                            egui::FontId::new(14.0, egui::FontFamily::Name(crate::icons::FAMILY.into())),
                            if item.bookmark { c.accent } else { c.muted },
                        );
                        let title = if item.title.is_empty() { strip(&item.url) } else { item.title.as_str() };
                        let title_rect = p.text(
                            egui::pos2(x + 26.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            title,
                            egui::FontId::proportional(13.5),
                            c.text,
                        );
                        p.text(
                            egui::pos2(title_rect.max.x + 10.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("— {}", item.url),
                            egui::FontId::proportional(12.5),
                            c.accent,
                        );
                        // Acting on the press: the address field loses the
                        // focus (and the list) before the release.
                        if resp.is_pointer_button_down_on() || resp.clicked() {
                            clicked = Some(item.url.clone());
                        }
                    }
                });
        });
    omni.rect = Some(area.response.rect);
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(url: &str) -> Suggestion {
        Suggestion { url: url.into(), title: String::new(), bookmark: false }
    }

    #[test]
    fn autofill_completes_the_site_then_the_path() {
        let items = [s("https://outlook.office.com/mail/inbox"), s("https://www.openai.com/")];
        assert_eq!(autofill("outl", &items).as_deref(), Some("ook.office.com/"));
        assert_eq!(autofill("OUTL", &items).as_deref(), Some("ook.office.com/"));
        assert_eq!(autofill("outlook.office.com/m", &items).as_deref(), Some("ail/inbox"));
        assert_eq!(autofill("open", &items).as_deref(), Some("ai.com/"));
        assert_eq!(autofill("www.op", &items).as_deref(), Some("enai.com/"));
        assert_eq!(autofill("https://outl", &items).as_deref(), Some("ook.office.com/"));
        assert_eq!(autofill("outlook.office.com/", &items).as_deref(), Some("mail/inbox"));
        assert_eq!(autofill("out look", &items), None);
        assert_eq!(autofill("xyz", &items), None);
    }

    #[test]
    fn steps_wrap_back_to_the_typed_text() {
        let mut o = Omnibox { items: vec![s("https://a/"), s("https://b/")], open: true, ..Default::default() };
        assert_eq!(o.step(true).map(|x| x.url.as_str()), Some("https://a/"));
        assert_eq!(o.step(true).map(|x| x.url.as_str()), Some("https://b/"));
        assert!(o.step(true).is_none());
        assert_eq!(o.step(false).map(|x| x.url.as_str()), Some("https://b/"));
    }
}
