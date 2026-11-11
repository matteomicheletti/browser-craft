//! The tab overview (Safari's "Show all tabs", Ctrl+Shift+\): a full-window
//! grid of tab pictures for every window, sectioned by window and by tab
//! group, with the split view's two tabs side by side. A search field
//! filters by title and address; a click (or Enter) goes to the tab, in
//! its own window; the x, a middle click or Ctrl+W closes it; the "+" tile
//! opens a new tab in this window.
//!
//! The overview is drawn by egui over the whole window; the web views are
//! hidden while it shows (it covers the page area, so no snapshot is
//! needed). The pictures come from `crate::thumbnails`.

use std::collections::HashMap;

use crate::egui_chrome::{self, Colors};
use crate::tab_groups::TabGroup;

/// The narrowest tile, in points.
pub const TILE_MIN: f32 = 220.0;
pub const TILE_MAX: f32 = 320.0;
pub const GAP: f32 = 18.0;
/// Picture area: width / height.
pub const ASPECT: f32 = 16.0 / 10.0;
const CAPTION_H: f32 = 34.0;

/// One tab, as another window describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct TabSummary {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub container: Option<String>,
    pub group: Option<u64>,
    pub split_with: Option<u64>,
    /// The window's active tab.
    pub active: bool,
    pub unloaded: bool,
}

/// One window: its tabs in strip order and its groups.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSummary {
    /// Stable id (`tabs.list`'s `window.id`).
    pub key: u64,
    /// Its name, or "Window N".
    pub label: String,
    pub tabs: Vec<TabSummary>,
    pub groups: Vec<TabGroup>,
}

/// A tile: one tab, or the split view's pair.
#[derive(Clone, Debug, PartialEq)]
pub enum Tile {
    Tab(TabSummary),
    Pair(TabSummary, TabSummary),
}

impl Tile {
    fn ids(&self) -> Vec<u64> {
        match self {
            Tile::Tab(t) => vec![t.id],
            Tile::Pair(a, b) => vec![a.id, b.id],
        }
    }

    /// Grid cells it takes.
    pub fn cells(&self) -> usize {
        match self {
            Tile::Tab(_) => 1,
            Tile::Pair(..) => 2,
        }
    }
}

/// Tabs of one group (or of no group) inside a window's section.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub group: Option<TabGroup>,
    pub tiles: Vec<Tile>,
}

/// A window's section.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub key: u64,
    pub label: String,
    /// The window the overview is shown in.
    pub current: bool,
    pub blocks: Vec<Block>,
    pub count: usize,
}

/// Whether a tab matches the search: every word of `query` is in its title
/// or address (case-insensitive).
pub fn matches(query: &str, title: &str, url: &str) -> bool {
    let (title, url) = (title.to_lowercase(), url.to_lowercase());
    query.split_whitespace().all(|w| {
        let w = w.to_lowercase();
        title.contains(&w) || url.contains(&w)
    })
}

/// The sections to draw: the current window first, then the others in
/// order; inside each, runs of tabs by group, and the split pair as one
/// tile. Windows with no matching tab are left out (except the current
/// one, which keeps its "+" tile).
pub fn sections(windows: &[WindowSummary], current: u64, query: &str) -> Vec<Section> {
    let mut order: Vec<&WindowSummary> = windows.iter().filter(|w| w.key == current).collect();
    order.extend(windows.iter().filter(|w| w.key != current));
    let mut out = Vec::new();
    for w in order {
        let shown: Vec<&TabSummary> = w.tabs.iter().filter(|t| matches(query, &t.title, &t.url)).collect();
        if shown.is_empty() && w.key != current {
            continue;
        }
        let mut blocks: Vec<Block> = Vec::new();
        let mut placed: Vec<u64> = Vec::new();
        for t in &shown {
            if placed.contains(&t.id) {
                continue;
            }
            let tile = match t.split_with.and_then(|o| shown.iter().find(|s| s.id == o)) {
                Some(other) => Tile::Pair((*t).clone(), (*other).clone()),
                None => Tile::Tab((*t).clone()),
            };
            placed.extend(tile.ids());
            let group = t.group.and_then(|g| w.groups.iter().find(|x| x.id == g)).cloned();
            match blocks.last_mut() {
                Some(b) if b.group.as_ref().map(|g| g.id) == group.as_ref().map(|g| g.id) => b.tiles.push(tile),
                _ => blocks.push(Block { group, tiles: vec![tile] }),
            }
        }
        out.push(Section { key: w.key, label: w.label.clone(), current: w.key == current, blocks, count: shown.len() });
    }
    out
}

/// How many columns fit in `width` points, and each tile's width.
pub fn grid(width: f32) -> (usize, f32) {
    let cols = (((width + GAP) / (TILE_MIN + GAP)).floor() as usize).max(1);
    let tile = ((width - GAP * (cols as f32 - 1.0)) / cols as f32).clamp(60.0, TILE_MAX);
    (cols, tile)
}

/// The tab ids in the order they are drawn (for the keyboard).
pub fn flat_order(sections: &[Section]) -> Vec<u64> {
    sections.iter().flat_map(|s| s.blocks.iter().flat_map(|b| b.tiles.iter().flat_map(Tile::ids))).collect()
}

/// Moves the keyboard selection: left/right one tab, up/down a row of
/// `cols`; nothing selected yet selects the first tab.
pub fn step(order: &[u64], selected: Option<u64>, delta: isize, cols: usize) -> Option<u64> {
    if order.is_empty() {
        return None;
    }
    let Some(at) = selected.and_then(|s| order.iter().position(|x| *x == s)) else {
        return order.first().copied();
    };
    let delta = match delta {
        d if d.abs() == 2 => d.signum() * cols.max(1) as isize,
        d => d,
    };
    let next = (at as isize + delta).clamp(0, order.len() as isize - 1) as usize;
    order.get(next).copied()
}

/// The overview's own state (per window).
#[derive(Default)]
pub struct Overview {
    pub open: bool,
    pub query: String,
    pub selected: Option<u64>,
    focus_search: bool,
    cols: usize,
}

/// What the user did in the overview.
#[derive(Clone, Debug, PartialEq)]
pub enum OverviewEvent {
    /// Go to this tab (in whatever window it is).
    Activate(u64),
    Close(u64),
    NewTab,
    Dismiss,
}

/// The pictures of a tab: its page picture (texture and pixel size) and
/// its favicon.
#[derive(Clone, Copy, Default)]
pub struct TileArt {
    pub thumb: Option<(egui::TextureId, egui::Vec2)>,
    pub favicon: Option<egui::TextureId>,
}

impl Overview {
    pub fn show(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = None;
        self.focus_search = true;
    }

    pub fn hide(&mut self) {
        self.open = false;
    }

    /// Draws the overview over `ui` (the whole window). `art` has the
    /// pictures by tab id.
    pub fn ui(&mut self, ui: &mut egui::Ui, c: &Colors, windows: &[WindowSummary], current: u64, art: &HashMap<u64, TileArt>) -> Vec<OverviewEvent> {
        let mut out = Vec::new();
        let sections = sections(windows, current, &self.query);
        let order = flat_order(&sections);
        if self.selected.is_some_and(|s| !order.contains(&s)) {
            self.selected = None;
        }
        // Keys: arrows move, Enter goes, Escape clears or leaves.
        let (left, right, up, down, enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let cols = self.cols.max(1);
        for (pressed, delta) in [(left, -1), (right, 1), (up, -2), (down, 2)] {
            if pressed {
                self.selected = step(&order, self.selected, delta, cols);
            }
        }
        if enter && let Some(id) = self.selected.or_else(|| order.first().copied()) {
            out.push(OverviewEvent::Activate(id));
        }
        if escape {
            if self.query.is_empty() {
                out.push(OverviewEvent::Dismiss);
            } else {
                self.query.clear();
            }
        }

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, c.sidebar_bg);
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(full.shrink2(egui::vec2(28.0, 16.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut content;
        // Header: search field, count, done.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("All tabs").size(18.0).strong().color(c.text));
            ui.add_space(16.0);
            let search_id = egui::Id::new("bc-overview-search");
            let edit = egui::TextEdit::singleline(&mut self.query)
                .id(search_id)
                .hint_text("Search tabs by title or address")
                .desired_width(320.0)
                .margin(egui::vec2(10.0, 6.0));
            let resp = ui.add(edit);
            if self.focus_search {
                resp.request_focus();
                self.focus_search = false;
            }
            if resp.changed() {
                self.selected = None;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let done = egui::Button::new(egui::RichText::new("Done").size(13.0).color(c.text)).corner_radius(egui::CornerRadius::same(8));
                if ui.add(done).on_hover_text("Back to the page (Esc)").clicked() {
                    out.push(OverviewEvent::Dismiss);
                }
                let total: usize = windows.iter().map(|w| w.tabs.len()).sum();
                let text = if windows.len() > 1 { format!("{total} tabs in {} windows", windows.len()) } else { format!("{total} tabs") };
                ui.label(egui::RichText::new(text).size(12.5).color(c.muted));
            });
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width();
            let (cols, tile_w) = grid(width);
            self.cols = cols;
            let tile_h = tile_w / ASPECT + CAPTION_H;
            for section in &sections {
                let suffix = if section.current { "  \u{b7}  this window" } else { "" };
                ui.label(egui::RichText::new(format!("{}{suffix}", section.label)).size(14.0).strong().color(if section.current { c.text } else { c.muted }));
                ui.add_space(6.0);
                for (bi, block) in section.blocks.iter().enumerate() {
                    if let Some(g) = &block.group {
                        let name = if g.name.is_empty() { "Unnamed group" } else { g.name.as_str() };
                        let color = c.group_color(g.color);
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 5.0, color);
                            ui.label(egui::RichText::new(name).size(12.5).strong().color(color));
                        });
                        ui.add_space(4.0);
                    }
                    let is_last = bi + 1 == section.blocks.len();
                    let new_tile = section.current && is_last && self.query.is_empty();
                    self.grid_ui(ui, c, &block.tiles, new_tile, cols, tile_w, tile_h, art, &mut out);
                    ui.add_space(10.0);
                }
                if section.blocks.is_empty() && section.current {
                    if self.query.is_empty() {
                        self.grid_ui(ui, c, &[], true, cols, tile_w, tile_h, art, &mut out);
                    } else {
                        ui.label(egui::RichText::new("No tab matches").size(13.0).color(c.muted));
                    }
                }
                ui.add_space(14.0);
            }
        });
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn grid_ui(&self, ui: &mut egui::Ui, c: &Colors, tiles: &[Tile], new_tile: bool, cols: usize, tile_w: f32, tile_h: f32, art: &HashMap<u64, TileArt>, out: &mut Vec<OverviewEvent>) {
        let origin = ui.cursor().min;
        let mut cell = 0usize;
        let place = |cell: usize| {
            let (row, col) = (cell / cols, cell % cols);
            egui::pos2(origin.x + col as f32 * (tile_w + GAP), origin.y + row as f32 * (tile_h + GAP))
        };
        for tile in tiles {
            // A pair never splits across rows.
            if tile.cells() == 2 && cols > 1 && cell % cols == cols - 1 {
                cell += 1;
            }
            let at = place(cell);
            match tile {
                Tile::Tab(t) => {
                    let rect = egui::Rect::from_min_size(at, egui::vec2(tile_w, tile_h));
                    self.tab_tile(ui, c, t, rect, art.get(&t.id).copied().unwrap_or_default(), out);
                }
                Tile::Pair(a, b) => {
                    let w2 = if cols > 1 { tile_w * 2.0 + GAP } else { tile_w };
                    let outer = egui::Rect::from_min_size(at, egui::vec2(w2, tile_h)).expand(5.0);
                    ui.painter().rect_stroke(outer, egui::CornerRadius::same(14), egui::Stroke::new(1.5, c.accent), egui::StrokeKind::Inside);
                    let half = if cols > 1 { tile_w } else { (tile_w - GAP) / 2.0 };
                    let ra = egui::Rect::from_min_size(at, egui::vec2(half, tile_h));
                    let rb = egui::Rect::from_min_size(egui::pos2(at.x + half + GAP, at.y), egui::vec2(half, tile_h));
                    self.tab_tile(ui, c, a, ra, art.get(&a.id).copied().unwrap_or_default(), out);
                    self.tab_tile(ui, c, b, rb, art.get(&b.id).copied().unwrap_or_default(), out);
                }
            }
            cell += if cols > 1 { tile.cells() } else { 1 };
        }
        if new_tile {
            let rect = egui::Rect::from_min_size(place(cell), egui::vec2(tile_w, tile_h));
            let resp = ui.interact(rect, egui::Id::new("bc-overview-new-tab"), egui::Sense::click());
            let fill = if resp.hovered() { c.hover } else { egui::Color32::TRANSPARENT };
            ui.painter().rect(rect, egui::CornerRadius::same(10), fill, egui::Stroke::new(1.0, c.line), egui::StrokeKind::Inside);
            ui.painter().text(rect.center() - egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, egui_chrome::ICON_PLUS, egui::FontId::new(26.0, egui::FontFamily::Name("phosphor".into())), c.muted);
            ui.painter().text(rect.center() + egui::vec2(0.0, 22.0), egui::Align2::CENTER_CENTER, "New tab", egui::FontId::proportional(13.0), c.muted);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                out.push(OverviewEvent::NewTab);
            }
            cell += 1;
        }
        let rows = cell.div_ceil(cols.max(1));
        let height = rows as f32 * (tile_h + GAP);
        ui.allocate_space(egui::vec2(ui.available_width(), (height - GAP).max(0.0)));
    }

    fn tab_tile(&self, ui: &mut egui::Ui, c: &Colors, t: &TabSummary, rect: egui::Rect, art: TileArt, out: &mut Vec<OverviewEvent>) {
        let resp = ui.interact(rect, egui::Id::new(("bc-overview-tile", t.id)), egui::Sense::click());
        let hovered = resp.hovered();
        let selected = self.selected == Some(t.id);
        let painter = ui.painter_at(rect.expand(4.0));
        let radius = egui::CornerRadius::same(10);
        painter.rect_filled(rect, radius, c.bar_bg);
        let image_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - CAPTION_H));
        let tint = if t.unloaded { egui::Color32::from_white_alpha(170) } else { egui::Color32::WHITE };
        match art.thumb {
            Some((tex, size)) => {
                // Cover the picture area.
                let scale = (image_rect.width() / size.x).max(image_rect.height() / size.y);
                let shown = egui::vec2(image_rect.width() / (size.x * scale), image_rect.height() / (size.y * scale));
                // The top left of the page, where its title and logo are.
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(shown.x.min(1.0), shown.y.min(1.0)));
                let corners = egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 };
                egui::Image::new((tex, image_rect.size())).uv(uv).corner_radius(corners).tint(tint).paint_at(ui, image_rect);
            }
            None => {
                painter.rect_filled(image_rect, egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, c.hover);
                let center = image_rect.center() - egui::vec2(0.0, 8.0);
                match art.favicon {
                    Some(tex) => {
                        let r = egui::Rect::from_center_size(center, egui::vec2(32.0, 32.0));
                        egui::Image::new((tex, r.size())).tint(tint).paint_at(ui, r);
                    }
                    None => {
                        painter.text(center, egui::Align2::CENTER_CENTER, egui_chrome::ICON_GLOBE, egui::FontId::new(30.0, egui::FontFamily::Name("phosphor".into())), c.muted);
                    }
                }
                let host = url::Url::parse(&t.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| t.url.clone());
                painter.text(center + egui::vec2(0.0, 30.0), egui::Align2::CENTER_CENTER, crate::browser::display_text(&host, 40), egui::FontId::proportional(12.0), c.muted);
            }
        }
        if let Some(ctr) = t.container.as_deref().and_then(crate::containers::get) {
            let line = egui::Rect::from_min_size(rect.min + egui::vec2(10.0, 0.0), egui::vec2(rect.width() - 20.0, 3.0));
            painter.rect_filled(line, egui::CornerRadius::same(2), ctr.color32());
        }
        // Caption: favicon and title.
        let caption = egui::Rect::from_min_max(egui::pos2(rect.min.x + 10.0, rect.max.y - CAPTION_H), egui::pos2(rect.max.x - 10.0, rect.max.y));
        let mut x = caption.min.x;
        if let Some(tex) = art.favicon {
            let r = egui::Rect::from_center_size(egui::pos2(x + 8.0, caption.center().y), egui::vec2(16.0, 16.0));
            egui::Image::new((tex, r.size())).tint(tint).paint_at(ui, r);
            x += 24.0;
        }
        let title = if t.title.trim().is_empty() { "Untitled" } else { t.title.as_str() };
        let galley = ui.painter().layout(
            title.to_string(),
            egui::FontId::proportional(12.5),
            if t.unloaded { c.muted } else { c.text },
            (caption.max.x - x).max(10.0),
        );
        let clipped = painter.with_clip_rect(egui::Rect::from_min_max(egui::pos2(x, caption.min.y), caption.max));
        clipped.galley(egui::pos2(x, caption.center().y - 8.0), galley, c.text);
        // Frame: selection, the window's active tab, hover.
        let stroke = if selected {
            egui::Stroke::new(2.5, c.accent)
        } else if t.active {
            egui::Stroke::new(1.5, c.accent.gamma_multiply(0.7))
        } else if hovered {
            egui::Stroke::new(1.5, c.muted)
        } else {
            egui::Stroke::new(1.0, c.line)
        };
        painter.rect_stroke(rect, radius, stroke, egui::StrokeKind::Outside);
        // Close button, on hover or selection.
        let mut closed = false;
        if hovered || selected {
            let r = egui::Rect::from_min_size(egui::pos2(rect.max.x - 30.0, rect.min.y + 8.0), egui::vec2(22.0, 22.0));
            let x_resp = ui.interact(r, egui::Id::new(("bc-overview-close", t.id)), egui::Sense::click());
            painter.circle_filled(r.center(), 11.0, if x_resp.hovered() { c.danger } else { c.bar_bg });
            painter.text(r.center(), egui::Align2::CENTER_CENTER, crate::icons::CLOSE, egui::FontId::new(12.0, egui::FontFamily::Name("phosphor".into())), if x_resp.hovered() { c.danger_text } else { c.text });
            if x_resp.on_hover_text("Close tab (Ctrl+W)").clicked() {
                out.push(OverviewEvent::Close(t.id));
                closed = true;
            }
        }
        if t.split_with.is_some() {
            let r = egui::Rect::from_min_size(egui::pos2(rect.min.x + 8.0, rect.min.y + 8.0), egui::vec2(22.0, 22.0));
            painter.circle_filled(r.center(), 11.0, c.bar_bg);
            painter.text(r.center(), egui::Align2::CENTER_CENTER, egui_phosphor::regular::COLUMNS, egui::FontId::new(12.0, egui::FontFamily::Name("phosphor".into())), c.accent);
        }
        if resp.middle_clicked() {
            out.push(OverviewEvent::Close(t.id));
        } else if resp.clicked() && !closed {
            out.push(OverviewEvent::Activate(t.id));
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u64, title: &str, url: &str) -> TabSummary {
        TabSummary { id, title: title.into(), url: url.into(), container: None, group: None, split_with: None, active: false, unloaded: false }
    }

    fn window(key: u64, tabs: Vec<TabSummary>) -> WindowSummary {
        WindowSummary { key, label: format!("Window {key}"), tabs, groups: Vec::new() }
    }

    #[test]
    fn search_matches_every_word_in_title_or_address() {
        assert!(matches("", "x", "y"));
        assert!(matches("rust", "The Rust Book", "https://doc.rust-lang.org/book"));
        assert!(matches("BOOK lang", "The Rust Book", "https://doc.rust-lang.org/"));
        assert!(!matches("python", "The Rust Book", "https://doc.rust-lang.org/"));
        assert!(matches("example.com", "Home", "https://example.com/"));
    }

    #[test]
    fn the_current_window_comes_first_and_empty_ones_are_hidden() {
        let windows = vec![window(1, vec![tab(1, "Alpha", "a")]), window(2, vec![tab(2, "Beta", "b"), tab(3, "Gamma", "g")])];
        let s = sections(&windows, 2, "");
        assert_eq!(s.iter().map(|s| s.key).collect::<Vec<_>>(), vec![2, 1]);
        assert!(s[0].current && !s[1].current);
        assert_eq!(s[0].count, 2);
        // Filtering leaves out the other window with no match, never the
        // current one.
        let s = sections(&windows, 1, "beta");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].key, 1);
        assert_eq!(s[0].count, 0);
        assert_eq!(flat_order(&s), vec![2]);
    }

    #[test]
    fn groups_make_blocks_and_split_pairs_one_tile() {
        let mut w = window(1, vec![tab(1, "a", "a"), tab(2, "b", "b"), tab(3, "c", "c"), tab(4, "d", "d")]);
        let mut groups = crate::tab_groups::TabGroups::default();
        let g = groups.create("Work", None);
        groups.add_tab(2, g, &[1, 2, 3, 4]);
        groups.add_tab(3, g, &[1, 2, 3, 4]);
        w.groups = groups.all().to_vec();
        w.tabs[1].group = Some(g);
        w.tabs[2].group = Some(g);
        w.tabs[0].split_with = Some(4);
        w.tabs[3].split_with = Some(1);
        let s = sections(&[w], 1, "");
        let blocks = &s[0].blocks;
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].group.is_none());
        assert!(matches!(&blocks[0].tiles[0], Tile::Pair(a, b) if a.id == 1 && b.id == 4));
        assert_eq!(blocks[1].group.as_ref().map(|g| g.name.as_str()), Some("Work"));
        assert_eq!(blocks[1].tiles.len(), 2);
        assert_eq!(flat_order(&s), vec![1, 4, 2, 3]);
    }

    #[test]
    fn the_grid_fits_the_width() {
        assert_eq!(grid(100.0).0, 1);
        let (cols, w) = grid(1000.0);
        assert_eq!(cols, 4);
        assert!(w >= TILE_MIN && w <= TILE_MAX);
        assert!(cols as f32 * w + (cols - 1) as f32 * GAP <= 1000.0 + 0.1);
    }

    #[test]
    fn arrows_walk_the_tiles() {
        let order = [10, 11, 12, 13, 14];
        assert_eq!(step(&order, None, 1, 3), Some(10));
        assert_eq!(step(&order, Some(10), 1, 3), Some(11));
        assert_eq!(step(&order, Some(10), -1, 3), Some(10), "stops at the first");
        assert_eq!(step(&order, Some(11), 2, 3), Some(14), "down a row");
        assert_eq!(step(&order, Some(13), 2, 3), Some(14), "stops at the last");
        assert_eq!(step(&order, Some(14), -2, 3), Some(11));
        assert_eq!(step(&[], Some(1), 1, 3), None);
    }
}

// ---- the window side: opening, drawing, page pictures ----

/// How long after a page loads its picture is taken (it paints first).
const THUMB_AFTER_LOAD: std::time::Duration = std::time::Duration::from_millis(1500);

impl super::Browser {
    /// Shows the overview (Ctrl+Shift+\), or leaves it.
    pub(super) fn toggle_overview(&mut self) {
        if self.overview.open {
            self.overview.hide();
        } else if !self.gpu_chrome {
            self.set_notice("The tab overview needs the GPU interface");
        } else {
            if self.feat.palette.is_open() {
                self.toggle_palette();
            }
            // A plugin modal is a web view stacked over everything: it would
            // sit on top of the grid.
            if self.plugin_modal_open() {
                self.close_plugin_modal(false);
            }
            self.overview.show();
            self.hide_hover_card();
            // A fresh picture of what was on screen.
            self.request_visible_thumbnails();
            self.take_keyboard();
        }
        self.request_redraw();
    }

    /// Asks for pictures of the pages on screen (the active tab and the
    /// split view's other pane).
    pub(super) fn request_visible_thumbnails(&self) {
        #[cfg(target_os = "linux")]
        {
            if let Some(super::WebPane::Wry(wv)) = &self.webview
                && self.web_page
            {
                super::request_thumbnail_gtk(wv, self.proxy.clone(), self.tab_id);
            }
            if let Some(peer) = self.split_partner()
                && let Some(super::WebPane::Wry(wv)) = self.tabs.iter().find(|t| t.id == peer).and_then(|t| t.view.as_ref())
            {
                super::request_thumbnail_gtk(wv, self.proxy.clone(), peer);
            }
        }
    }

    /// Asks for pictures of every live page (debug hook).
    #[cfg(debug_assertions)]
    pub(super) fn request_all_thumbnails(&self) {
        #[cfg(target_os = "linux")]
        for i in 0..self.tabs.len() {
            let id = self.tab_id_at(i);
            if let Some(super::WebPane::Wry(wv)) = self.view_of(id) {
                super::request_thumbnail_gtk(wv, self.proxy.clone(), id);
            }
        }
    }

    /// Takes tab `id`'s picture a moment from now (its page just loaded).
    pub(super) fn schedule_thumbnail(&mut self, id: u64) {
        let at = std::time::Instant::now() + THUMB_AFTER_LOAD;
        self.thumb_due.retain(|(_, t)| *t != id);
        self.thumb_due.push((at, id));
    }

    /// Takes the pictures that are due; returns when the next one is.
    pub(super) fn take_due_thumbnails(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        let due: Vec<u64> = self.thumb_due.iter().filter(|(at, _)| *at <= now).map(|(_, id)| *id).collect();
        self.thumb_due.retain(|(at, _)| *at > now);
        #[cfg(target_os = "linux")]
        for id in due {
            if let Some(super::WebPane::Wry(wv)) = self.view_of(id) {
                super::request_thumbnail_gtk(wv, self.proxy.clone(), id);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = due;
        self.thumb_due.iter().map(|(at, _)| *at).min()
    }

    /// Tab `id`'s page picture as a texture of this window (uploaded again
    /// when the picture changed).
    fn thumb_texture(&mut self, ctx: &egui::Context, id: u64) -> Option<(egui::TextureId, egui::Vec2)> {
        let shared = self.shared.clone();
        let mut sh = shared.borrow_mut();
        let thumb = sh.thumbs.get(id)?;
        let size = egui::vec2(thumb.w as f32, thumb.h as f32);
        if let Some((stamp, tex)) = self.thumb_tex.get(&id)
            && *stamp == thumb.stamp
        {
            return Some((tex.id(), size));
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([thumb.w, thumb.h], &thumb.rgba);
        let tex = ctx.load_texture(format!("thumb-{id}"), image, egui::TextureOptions::LINEAR);
        let out = (tex.id(), size);
        self.thumb_tex.insert(id, (thumb.stamp, tex));
        Some(out)
    }

    /// Tab `id`'s favicon as a texture of this window (it may belong to a
    /// tab of another window).
    fn favicon_texture(&mut self, ctx: &egui::Context, id: u64) -> Option<egui::TextureId> {
        if let Some(t) = self.favicons.get(&id) {
            return Some(t.id());
        }
        let icon = self.shared.borrow().favicons.get(&id).cloned()?;
        let image = egui::ColorImage::from_rgba_unmultiplied([icon.0, icon.1], &icon.2);
        let tex = ctx.load_texture(format!("favicon-{id}"), image, egui::TextureOptions::LINEAR);
        let out = tex.id();
        self.favicons.insert(id, tex);
        Some(out)
    }

    /// Draws the overview over the whole window and applies what the user
    /// did in it.
    pub(super) fn overview_frame(&mut self, ui: &mut egui::Ui, c: &Colors, titlebar: bool, maximized: bool, edge: f32) {
        let mut windows = self.shared.borrow().summaries.clone();
        if !windows.iter().any(|w| w.key == self.win_key) {
            windows = vec![self.summary(0)];
        }
        let ids: Vec<u64> = windows.iter().flat_map(|w| w.tabs.iter().map(|t| t.id)).collect();
        let mut art = HashMap::new();
        let ctx = ui.ctx().clone();
        for id in &ids {
            let thumb = self.thumb_texture(&ctx, *id);
            let favicon = self.favicon_texture(&ctx, *id);
            art.insert(*id, TileArt { thumb, favicon });
        }
        self.thumb_tex.retain(|id, _| ids.contains(id));
        let mut win_ops = Vec::new();
        let mut events = Vec::new();
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(c.sidebar_bg)).show(ui, |ui| {
            let full = ui.max_rect();
            if titlebar {
                // The header row still moves the window.
                let bar = full.with_max_y(full.min.y + 12.0);
                crate::titlebar::drag_area(ui, bar, c, maximized, &mut win_ops);
            }
            events = self.overview.ui(ui, c, &windows, self.win_key, &art);
        });
        if titlebar {
            crate::titlebar::window_edges(ui.ctx(), c, edge, &mut win_ops);
        }
        for op in win_ops {
            self.window_op(op);
        }
        for event in events {
            self.overview_event(event);
        }
    }

    fn overview_event(&mut self, event: OverviewEvent) {
        match event {
            OverviewEvent::Activate(id) => {
                self.overview.hide();
                match self.tab_index(id) {
                    Some(i) => self.switch_tab(i),
                    None => self.app_requests.push(super::windows::AppRequest::ActivateTab(id)),
                }
            }
            OverviewEvent::Close(id) => match self.tab_index(id) {
                Some(_) => {
                    if self.tabs.len() == 1 {
                        // The window's last tab: a fresh one keeps the
                        // window (and the overview) open.
                        self.new_tab();
                    }
                    if let Some(i) = self.tab_index(id) {
                        self.close_tab(i);
                    }
                }
                None => self.app_requests.push(super::windows::AppRequest::CloseTab(id)),
            },
            OverviewEvent::NewTab => {
                self.overview.hide();
                self.new_tab();
            }
            OverviewEvent::Dismiss => self.overview.hide(),
        }
        self.request_redraw();
    }

    /// Keys while the overview shows: Ctrl+W closes the selected tab,
    /// Ctrl+Shift+\ leaves; another shortcut leaves and runs. Returns
    /// whether the key was taken (egui gets the rest: arrows, Enter, Esc,
    /// typing in the search field).
    pub(super) fn overview_key(&mut self, base: &winit::keyboard::Key, ctrl: bool, shift: bool, alt: bool) -> bool {
        match super::shortcut_for(base, ctrl, shift, alt, true) {
            Some(super::Shortcut::CloseTab) => {
                if let Some(id) = self.overview.selected {
                    self.overview_event(OverviewEvent::Close(id));
                }
                true
            }
            Some(super::Shortcut::Command(crate::commands::CommandId::ShowAllTabs)) => {
                self.toggle_overview();
                true
            }
            Some(_) => {
                self.overview.hide();
                false
            }
            None => {
                self.request_redraw();
                true
            }
        }
    }
}
