//! The Library sidebar (Ctrl+B): bookmarks with their folders and the
//! browsing history, in a resizable panel on the left of the page.
//!
//! The panel only draws and reports [`LibraryEvent`]s; the browser opens
//! pages and changes the stores.

use std::collections::HashSet;

use crate::bookmarks::{self, Node};
use super::ContainerChoice;
use crate::containers::Container;
use crate::egui_chrome::{self, Colors};
use crate::flags::LibrarySection;
use crate::history::{self, VisitRow};
use crate::icons;

/// History rows loaded at a time ("Load more" adds as many again).
pub const HISTORY_PAGE: usize = 200;
/// The most bookmarks a search lists.
const SEARCH_MAX: usize = 500;
const ROW_H: f32 = 26.0;
const INDENT: f32 = 14.0;

/// What the user asked for in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryEvent {
    Open(String),
    OpenInNewTab(String),
    OpenIn(String, ContainerChoice),
    Section(LibrarySection),
    DeleteBookmark(u64),
    NewFolder(u64, String),
    RemoveHistory(String),
    Close,
}

/// The sidebar's state for one window (kept in memory only).
pub struct Library {
    /// The folders shown open.
    expanded: HashSet<u64>,
    pub query: String,
    history: Vec<VisitRow>,
    history_more: bool,
    /// The query the history rows were loaded for (`None`: reload).
    loaded_for: Option<String>,
    /// A folder waiting for "Delete" to be confirmed: (id, title, links).
    confirm_delete: Option<(u64, String, usize)>,
    /// A folder being created: (parent, name).
    new_folder: Option<(u64, String)>,
    pub focus_search: bool,
    /// Just opened: the browser draws one more frame.
    pub fresh: bool,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            // The bookmarks bar folder starts open, like Chrome's.
            expanded: HashSet::from([bookmarks::BAR_ID]),
            query: String::new(),
            history: Vec::new(),
            history_more: false,
            loaded_for: None,
            confirm_delete: None,
            new_folder: None,
            focus_search: false,
            fresh: true,
        }
    }
}

/// History rows split by day, newest first: (heading, rows).
pub fn group_by_day(rows: &[VisitRow]) -> Vec<(String, Vec<&VisitRow>)> {
    let mut out: Vec<(i64, String, Vec<&VisitRow>)> = Vec::new();
    for row in rows {
        let (day, heading) = history::day_heading(row.time);
        match out.last_mut() {
            Some((d, _, list)) if *d == day => list.push(row),
            _ => out.push((day, heading, vec![row])),
        }
    }
    out.into_iter().map(|(_, h, l)| (h, l)).collect()
}

/// Whether a click opens in a new tab: middle click or Ctrl/Cmd + click.
fn wants_new_tab(ui: &egui::Ui, resp: &egui::Response) -> bool {
    resp.middle_clicked() || (resp.clicked() && ui.input(|i| i.modifiers.command))
}

/// One line of text cut to `width` with an ellipsis.
fn one_line(ui: &egui::Ui, text: &str, size: f32, color: egui::Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(egui::FontId::proportional(size), color),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(10.0));
    ui.painter().layout_job(job)
}

/// A clickable row: an optional caret, an icon, the title and a muted
/// second text on the right (or under it when `two_lines`).
struct Row<'a> {
    depth: usize,
    caret: Option<bool>,
    glyph: &'a str,
    title: &'a str,
    detail: &'a str,
    two_lines: bool,
}

fn row(ui: &mut egui::Ui, c: &Colors, r: Row) -> egui::Response {
    let h = if r.two_lines { ROW_H + 14.0 } else { ROW_H };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = ui.painter();
    if resp.hovered() || resp.context_menu_opened() {
        p.rect_filled(rect, 6.0, c.hover);
    }
    let mut x = rect.min.x + 6.0 + r.depth as f32 * INDENT;
    let top = if r.two_lines { rect.min.y + ROW_H / 2.0 } else { rect.center().y };
    let icon_font = egui::FontId::new(13.0, icons::family());
    if let Some(open) = r.caret {
        let caret = if open { egui_phosphor::regular::CARET_DOWN } else { egui_phosphor::regular::CARET_RIGHT };
        p.text(egui::pos2(x, top), egui::Align2::LEFT_CENTER, caret, egui::FontId::new(11.0, icons::family()), c.muted);
    }
    x += 14.0;
    let icon_color = if r.caret.is_some() { c.accent } else { c.muted };
    p.text(egui::pos2(x, top), egui::Align2::LEFT_CENTER, r.glyph, icon_font, icon_color);
    x += 20.0;
    let right = rect.max.x - 6.0;
    if r.two_lines {
        let title = one_line(ui, r.title, 13.0, c.text, right - x);
        p.galley(egui::pos2(x, top - title.size().y / 2.0), title, c.text);
        let detail = one_line(ui, r.detail, 11.5, c.muted, right - x);
        p.galley(egui::pos2(x, top + 8.0), detail, c.muted);
    } else {
        let detail = one_line(ui, r.detail, 11.5, c.muted, 80.0);
        let dw = if r.detail.is_empty() { 0.0 } else { detail.size().x + 8.0 };
        let title = one_line(ui, r.title, 13.0, c.text, right - x - dw);
        p.galley(egui::pos2(x, top - title.size().y / 2.0), title, c.text);
        if dw > 0.0 {
            p.galley(egui::pos2(right - detail.size().x, top - detail.size().y / 2.0), detail, c.muted);
        }
    }
    resp
}

/// The right-click entries shared by bookmarks and history rows.
fn open_menu(ui: &mut egui::Ui, c: &Colors, url: &str, containers: &[Container], out: &mut Vec<LibraryEvent>) {
    if egui_chrome::menu_item(ui, c, egui_phosphor::regular::ARROW_SQUARE_OUT, "Open in new tab").clicked() {
        out.push(LibraryEvent::OpenInNewTab(url.to_string()));
        ui.close();
    }
    ui.menu_button(
        egui::RichText::new(format!("{}  Open in container", egui_phosphor::regular::SQUARES_FOUR)).size(13.0).color(c.text),
        |ui| {
            if let Some(pick) = egui_chrome::container_menu(ui, c, containers, "No container") {
                out.push(LibraryEvent::OpenIn(url.to_string(), pick));
            }
        },
    );
}

impl Library {
    /// Reloads the history from its first page the next time it shows.
    pub fn invalidate_history(&mut self) {
        self.loaded_for = None;
    }

    fn load_history(&mut self) {
        let q = self.query.trim().to_string();
        if self.loaded_for.as_deref() == Some(q.as_str()) {
            return;
        }
        (self.history, self.history_more) = history::visits(&q, 0, HISTORY_PAGE);
        self.loaded_for = Some(q);
    }

    fn load_more(&mut self) {
        let q = self.query.trim().to_string();
        let (more_rows, more) = history::visits(&q, self.history.len(), HISTORY_PAGE);
        self.history.extend(more_rows);
        self.history_more = more;
    }

    /// Draws the sidebar's contents.
    pub fn ui(&mut self, ui: &mut egui::Ui, c: &Colors, section: LibrarySection, containers: &[Container]) -> Vec<LibraryEvent> {
        let mut out = Vec::new();
        // Header: Bookmarks | History, and close.
        ui.horizontal(|ui| {
            for (s, label) in [(LibrarySection::Bookmarks, "Bookmarks"), (LibrarySection::History, "History")] {
                let on = s == section;
                let text = egui::RichText::new(label).size(13.0).color(if on { c.accent_text } else { c.text });
                let button = egui::Button::new(text)
                    .fill(if on { c.accent } else { c.hover })
                    .corner_radius(egui::CornerRadius::same(8))
                    .min_size(egui::vec2(0.0, 28.0));
                if ui.add(button).clicked() && !on {
                    out.push(LibraryEvent::Section(s));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if egui_chrome::icon_button(ui, true, egui_chrome::ICON_XMARK, "Close sidebar (Ctrl+B)", 13.0).clicked() {
                    out.push(LibraryEvent::Close);
                }
            });
        });
        ui.add_space(6.0);
        let hint = match section {
            LibrarySection::Bookmarks => "Search bookmarks",
            LibrarySection::History => "Search history",
        };
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .id(egui::Id::new("bc-library-search"))
                .hint_text(hint)
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_search) {
            search.request_focus();
        }
        if search.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.query.clear();
        }
        ui.add_space(6.0);
        match section {
            LibrarySection::Bookmarks => self.bookmarks_ui(ui, c, containers, &mut out),
            LibrarySection::History => self.history_ui(ui, c, containers, &mut out),
        }
        out
    }

    fn bookmarks_ui(&mut self, ui: &mut egui::Ui, c: &Colors, containers: &[Container], out: &mut Vec<LibraryEvent>) {
        let tree = bookmarks::snapshot();
        if let Some((id, title, links)) = self.confirm_delete.clone() {
            ui.label(egui::RichText::new(format!("Delete the folder \"{title}\" and its {links} bookmarks?")).size(12.5).color(c.text));
            ui.horizontal(|ui| {
                let delete = egui::Button::new(egui::RichText::new("Delete").color(c.danger_text)).fill(c.danger);
                if ui.add(delete).clicked() {
                    out.push(LibraryEvent::DeleteBookmark(id));
                    self.confirm_delete = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_delete = None;
                }
            });
            ui.add_space(6.0);
        }
        if let Some((parent, mut name)) = self.new_folder.take() {
            let in_folder = tree.find(parent).map_or(String::new(), |n| n.title.clone());
            ui.label(egui::RichText::new(format!("New folder in \"{in_folder}\"")).size(12.5).color(c.muted));
            let mut keep = true;
            ui.horizontal(|ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Folder name").desired_width(ui.available_width() - 70.0));
                if !field.has_focus() && name.is_empty() {
                    field.request_focus();
                }
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Create").clicked() || enter {
                    out.push(LibraryEvent::NewFolder(parent, name.clone()));
                    self.expanded.insert(parent);
                    keep = false;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
            if keep {
                self.new_folder = Some((parent, name));
            }
            ui.add_space(6.0);
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let q = self.query.trim();
            if q.is_empty() {
                for root in [&tree.bar, &tree.other] {
                    self.node_ui(ui, c, root, root.id, 0, containers, out);
                }
                return;
            }
            let hits = tree.search(q, SEARCH_MAX);
            if hits.is_empty() {
                ui.label(egui::RichText::new("No bookmarks match.").size(12.5).color(c.muted));
            }
            for e in &hits {
                let resp = row(ui, c, Row { depth: 0, caret: None, glyph: icons::GLOBE, title: &e.title, detail: &e.folder, two_lines: true })
                    .on_hover_text(&e.url);
                if wants_new_tab(ui, &resp) {
                    out.push(LibraryEvent::OpenInNewTab(e.url.clone()));
                } else if resp.clicked() {
                    out.push(LibraryEvent::Open(e.url.clone()));
                }
                resp.context_menu(|ui| {
                    open_menu(ui, c, &e.url, containers, out);
                    if egui_chrome::menu_item(ui, c, icons::TRASH, "Delete").clicked() {
                        out.push(LibraryEvent::DeleteBookmark(e.id));
                        ui.close();
                    }
                });
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn node_ui(&mut self, ui: &mut egui::Ui, c: &Colors, node: &Node, parent: u64, depth: usize, containers: &[Container], out: &mut Vec<LibraryEvent>) {
        let root = node.id == bookmarks::BAR_ID || node.id == bookmarks::OTHER_ID;
        match &node.url {
            None => {
                let open = self.expanded.contains(&node.id);
                let count = node.link_count().to_string();
                let resp = row(ui, c, Row { depth, caret: Some(open), glyph: icons::FOLDER, title: &node.title, detail: &count, two_lines: false });
                if resp.clicked() {
                    if open {
                        self.expanded.remove(&node.id);
                    } else {
                        self.expanded.insert(node.id);
                    }
                }
                resp.context_menu(|ui| {
                    if egui_chrome::menu_item(ui, c, egui_phosphor::regular::FOLDER_SIMPLE_PLUS, "New folder").clicked() {
                        self.new_folder = Some((node.id, String::new()));
                        ui.close();
                    }
                    if !root && egui_chrome::menu_item(ui, c, icons::TRASH, "Delete folder…").clicked() {
                        self.confirm_delete = Some((node.id, node.title.clone(), node.link_count()));
                        ui.close();
                    }
                });
                if open {
                    if node.children.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(40.0 + (depth + 1) as f32 * INDENT);
                            ui.label(egui::RichText::new("(empty)").size(12.0).color(c.muted));
                        });
                    }
                    for child in &node.children {
                        self.node_ui(ui, c, child, node.id, depth + 1, containers, out);
                    }
                }
            }
            Some(url) => {
                let resp = row(ui, c, Row { depth, caret: None, glyph: icons::GLOBE, title: &node.title, detail: "", two_lines: false })
                    .on_hover_text(url);
                if wants_new_tab(ui, &resp) {
                    out.push(LibraryEvent::OpenInNewTab(url.clone()));
                } else if resp.clicked() {
                    out.push(LibraryEvent::Open(url.clone()));
                }
                resp.context_menu(|ui| {
                    open_menu(ui, c, url, containers, out);
                    if egui_chrome::menu_item(ui, c, egui_phosphor::regular::FOLDER_SIMPLE_PLUS, "New folder").clicked() {
                        self.new_folder = Some((parent, String::new()));
                        ui.close();
                    }
                    if egui_chrome::menu_item(ui, c, icons::TRASH, "Delete").clicked() {
                        out.push(LibraryEvent::DeleteBookmark(node.id));
                        ui.close();
                    }
                });
            }
        }
    }

    fn history_ui(&mut self, ui: &mut egui::Ui, c: &Colors, containers: &[Container], out: &mut Vec<LibraryEvent>) {
        self.load_history();
        let mut load_more = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            if self.history.is_empty() {
                let text = if self.query.trim().is_empty() { "No history yet." } else { "Nothing in your history matches." };
                ui.label(egui::RichText::new(text).size(12.5).color(c.muted));
            }
            for (heading, rows) in group_by_day(&self.history) {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(heading).size(12.0).strong().color(c.muted));
                for v in rows {
                    let title = if v.title.is_empty() { v.url.as_str() } else { v.title.as_str() };
                    let time = history::time_of_day(v.time);
                    let resp = row(ui, c, Row { depth: 0, caret: None, glyph: icons::GLOBE, title, detail: &time, two_lines: false })
                        .on_hover_text(&v.url);
                    if wants_new_tab(ui, &resp) {
                        out.push(LibraryEvent::OpenInNewTab(v.url.clone()));
                    } else if resp.clicked() {
                        out.push(LibraryEvent::Open(v.url.clone()));
                    }
                    resp.context_menu(|ui| {
                        open_menu(ui, c, &v.url, containers, out);
                        if egui_chrome::menu_item(ui, c, icons::TRASH, "Remove from history").clicked() {
                            out.push(LibraryEvent::RemoveHistory(v.url.clone()));
                            ui.close();
                        }
                    });
                }
            }
            if self.history_more {
                ui.add_space(6.0);
                if ui.button("Load more").clicked() {
                    load_more = true;
                }
            }
        });
        if load_more {
            self.load_more();
        }
        if out.iter().any(|e| matches!(e, LibraryEvent::RemoveHistory(_))) {
            self.invalidate_history();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_rows_group_by_day_newest_first() {
        let now = history::now_us();
        let day = 86_400_000_000;
        let v = |time: i64| VisitRow { url: "https://a.test/".into(), title: "A".into(), time };
        let rows = [v(now), v(now - 1), v(now - 2 * day), v(now - 2 * day - 5)];
        let groups = group_by_day(&rows);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Today");
        assert_eq!((groups[0].1.len(), groups[1].1.len()), (2, 2));
        assert!(groups[1].0.contains(char::is_numeric), "older days show the date: {}", groups[1].0);
    }

    #[test]
    fn the_bookmarks_bar_starts_open() {
        let lib = Library::default();
        assert!(lib.expanded.contains(&bookmarks::BAR_ID));
        assert!(lib.loaded_for.is_none());
    }
}
