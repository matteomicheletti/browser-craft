//! Tab groups and split view in the browser window. The models live in
//! `crate::tab_groups` and `crate::split`; this file applies them to the
//! tabs (order, activation, web views) and draws the split panes.

use std::collections::HashSet;

use super::{Browser, Rect, TabState, WebPane};
use crate::egui_chrome::{Colors, GroupEvent};
use crate::palette::GroupItem;
use crate::split::{self, Side, Split};
use crate::tab_groups::StripItem;

/// The second pane of a split view: the partner of the active tab, whose
/// web view stays in its `TabState` and is placed here.
#[derive(Default)]
pub(super) struct SplitPeer {
    /// The tab whose web view is shown as the second pane.
    shown: Option<u64>,
    visible: bool,
    bounds: Option<(i32, i32, u32, u32)>,
    /// Where the second pane goes (physical pixels), from the egui layout.
    pub(super) rect: Option<Rect>,
    /// A picture of the second pane while a menu covers the page.
    pub(super) snapshot: Option<egui::TextureHandle>,
    snapshot_pending: bool,
    /// The tab that was active when it was parked: its web view stayed
    /// visible in case it becomes the second pane; hidden otherwise.
    pub(super) maybe_hide: Option<u64>,
    /// A web view was already created for this tab: never retried in a loop.
    load_tried: Option<u64>,
}

/// What the split panes drawn in the page area report.
pub(super) struct PanesOut {
    /// The focused pane (points).
    pub focus: egui::Rect,
    /// The other pane (points).
    pub peer: Option<egui::Rect>,
    pub ratio: Option<f32>,
    /// The other pane's placeholder (an engine page) was clicked.
    pub focus_peer: bool,
}

/// Lays out the page area for a split view and draws what egui owns there:
/// the divider (dragged to resize, double-click to even out), the focus
/// marks above the panes and, for a partner tab with no web view, a
/// placeholder. `layout` is the split and the focused pane's side.
pub(super) fn split_panes(
    ui: &mut egui::Ui,
    c: &Colors,
    area: egui::Rect,
    layout: Option<(Split, Side)>,
    peer_title: &str,
    peer_placeholder: bool,
    peer_snapshot: Option<egui::TextureId>,
) -> PanesOut {
    let Some((s, side)) = layout else {
        return PanesOut { focus: area, peer: None, ratio: None, focus_peer: false };
    };
    const MARK: f32 = 3.0;
    let [l, d, r] = s.spans(area.min.x, area.width(), split::DIVIDER);
    let pane = |(x, w): (f32, f32)| egui::Rect::from_min_max(egui::pos2(x, area.min.y + MARK), egui::pos2(x + w, area.max.y));
    let mark = |(x, w): (f32, f32)| egui::Rect::from_min_max(egui::pos2(x, area.min.y), egui::pos2(x + w, area.min.y + MARK));
    let (focus_span, peer_span) = if side == Side::Left { (l, r) } else { (r, l) };
    let painter = ui.painter().clone();
    painter.rect_filled(mark(focus_span), 0.0, c.accent);
    painter.rect_filled(mark(peer_span), 0.0, c.line);
    let divider = egui::Rect::from_min_max(egui::pos2(d.0, area.min.y), egui::pos2(d.0 + d.1, area.max.y));
    let resp = ui
        .interact(divider, egui::Id::new("bc-split-divider"), egui::Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to resize, double-click to even out");
    let active = resp.hovered() || resp.dragged();
    painter.rect_filled(divider, 0.0, if active { c.hover } else { c.sidebar_bg });
    let grip = egui::Rect::from_center_size(divider.center(), egui::vec2(2.0, 28.0));
    painter.rect_filled(grip, egui::CornerRadius::same(1), if active { c.accent } else { c.muted });
    let mut ratio = None;
    if resp.double_clicked() {
        ratio = Some(0.5);
    } else if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        ratio = Some(Split::ratio_at(p.x, area.min.x, area.width(), split::DIVIDER));
    }
    let peer = pane(peer_span);
    let mut focus_peer = false;
    if let Some(tex) = peer_snapshot {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex, peer, uv, egui::Color32::WHITE);
    } else if peer_placeholder {
        painter.rect_filled(peer, 0.0, c.bar_bg);
        painter.text(peer.center() - egui::vec2(0.0, 12.0), egui::Align2::CENTER_CENTER, peer_title, egui::FontId::proportional(15.0), c.text);
        painter.text(peer.center() + egui::vec2(0.0, 12.0), egui::Align2::CENTER_CENTER, "Click to show this tab here", egui::FontId::proportional(12.5), c.muted);
        focus_peer = ui.interact(peer, egui::Id::new("bc-split-peer"), egui::Sense::click()).clicked();
    }
    PanesOut { focus: pane(focus_span), peer: Some(peer), ratio, focus_peer }
}

impl Browser {
    // ---- tab order ----

    /// The tab ids in strip order.
    pub(super) fn tab_order(&self) -> Vec<u64> {
        (0..self.tabs.len()).map(|i| self.tab_id_at(i)).collect()
    }

    /// Reorders the tabs to `order` (a permutation of `tab_order`); the
    /// active tab stays active.
    pub(super) fn apply_order(&mut self, order: &[u64]) {
        let current = self.tab_order();
        let unique: HashSet<u64> = order.iter().copied().collect();
        if order == current.as_slice() || order.len() != current.len() || unique.len() != order.len() || !order.iter().all(|id| current.contains(id)) {
            return;
        }
        let mut slots: Vec<Option<TabState>> = std::mem::take(&mut self.tabs).into_iter().map(Some).collect();
        self.tabs = order
            .iter()
            .map(|id| current.iter().position(|c| c == id).and_then(|p| slots[p].take()).unwrap_or_default())
            .collect();
        self.active = order.iter().position(|id| *id == self.tab_id).unwrap_or(0);
        self.request_redraw();
    }

    // ---- groups ----

    /// What the strip and the sidebar show, in order.
    pub(super) fn strip_items(&self) -> Vec<StripItem> {
        self.groups.strip(&self.tab_order())
    }

    /// The groups and their tab counts (owned: the strip is drawn while
    /// the browser is borrowed mutably).
    pub(super) fn group_snapshot(&self) -> Vec<(crate::tab_groups::TabGroup, usize)> {
        let order = self.tab_order();
        self.groups.all().iter().map(|g| (g.clone(), self.groups.members(g.id, &order).len())).collect()
    }

    pub(super) fn palette_groups(&self, c: &Colors) -> Vec<GroupItem> {
        let order = self.tab_order();
        self.groups
            .all()
            .iter()
            .map(|g| GroupItem {
                id: g.id,
                name: g.name.clone(),
                color: c.group_color(g.color),
                tabs: self.groups.members(g.id, &order).len(),
                collapsed: g.collapsed,
            })
            .collect()
    }

    /// The group colour of tab `i`, for the strip.
    pub(super) fn tab_group_mark(&self, i: usize, c: &Colors) -> Option<(u64, egui::Color32)> {
        self.groups.group_of(self.tab_id_at(i)).map(|g| (g.id, c.group_color(g.color)))
    }

    /// Puts tab `i` in a new group and opens its editor.
    pub(super) fn group_new_from_tab(&mut self, i: usize) {
        let tab = self.tab_id_at(i);
        let order = self.groups.remove_tab(tab, &self.tab_order());
        let group = self.groups.create("", None);
        let order = self.groups.add_tab(tab, group, &order);
        self.apply_order(&order);
        self.edit_group = Some(group);
        self.request_redraw();
    }

    pub(super) fn group_add_tab(&mut self, i: usize, group: u64) {
        let tab = self.tab_id_at(i);
        let order = self.groups.add_tab(tab, group, &self.tab_order());
        self.apply_order(&order);
        // A tab joining a collapsed group shows it.
        if i == self.active {
            self.groups.set_collapsed(group, false);
        }
    }

    pub(super) fn group_remove_tab(&mut self, i: usize) {
        let tab = self.tab_id_at(i);
        let order = self.groups.remove_tab(tab, &self.tab_order());
        self.apply_order(&order);
    }

    /// Showing tab `i` expands its collapsed group (as Chrome does).
    pub(super) fn expand_group_of(&mut self, i: usize) {
        if let Some(g) = self.groups.group_of(self.tab_id_at(i)).filter(|g| g.collapsed).map(|g| g.id) {
            self.groups.set_collapsed(g, false);
        }
    }

    /// Collapses or expands `group`. Collapsing the active tab's group
    /// moves to the nearest tab outside it (a new tab when there is none).
    /// Unloaded tabs stay unloaded either way.
    pub(super) fn set_group_collapsed(&mut self, group: u64, collapsed: bool) {
        if !self.groups.set_collapsed(group, collapsed) || !collapsed {
            return;
        }
        if self.groups.group_of(self.tab_id).is_none_or(|g| g.id != group) {
            return;
        }
        let order = self.tab_order();
        let shown = |i: &usize| !self.groups.group_of(order[*i]).is_some_and(|g| g.collapsed);
        let target = (self.active + 1..order.len()).find(shown).or_else(|| (0..self.active).rev().find(shown));
        match target {
            Some(i) => self.switch_tab(i),
            None => self.new_tab(),
        }
    }

    pub(super) fn group_event(&mut self, group: u64, event: GroupEvent) {
        match event {
            GroupEvent::ToggleCollapsed => {
                let collapsed = self.groups.get(group).is_some_and(|g| g.collapsed);
                self.set_group_collapsed(group, !collapsed);
            }
            GroupEvent::Rename(name) => {
                self.groups.rename(group, &name);
            }
            GroupEvent::Color(color) => {
                self.groups.set_color(group, color);
            }
            GroupEvent::NewTab => {
                self.new_tab();
                let order = self.groups.add_tab(self.tab_id, group, &self.tab_order());
                self.apply_order(&order);
                self.groups.set_collapsed(group, false);
            }
            GroupEvent::Ungroup => self.groups.ungroup(group),
            GroupEvent::Close => {
                let members = self.groups.members(group, &self.tab_order());
                if members.len() == self.tabs.len() {
                    // Closing every tab would quit: keep a fresh one.
                    self.new_tab();
                }
                for id in members {
                    if let Some(i) = self.tab_index(id) {
                        self.close_tab(i);
                    }
                }
            }
        }
        self.request_redraw();
    }

    /// Palette: go to a group, expanding it (its first tab, or the active
    /// one when it is in the group already).
    pub(super) fn focus_group(&mut self, group: u64) {
        self.groups.set_collapsed(group, false);
        let members = self.groups.members(group, &self.tab_order());
        if !members.contains(&self.tab_id)
            && let Some(i) = members.first().and_then(|id| self.tab_index(*id))
        {
            self.switch_tab(i);
        }
        self.request_redraw();
    }

    /// A tab closed: it leaves its group and the split view.
    pub(super) fn forget_closed_tab(&mut self, id: u64) {
        self.groups.forget_tab(id);
        self.split = self.split.and_then(|s| s.after_close(id));
        if self.edit_group.is_some_and(|g| self.groups.get(g).is_none()) {
            self.edit_group = None;
        }
    }

    // ---- split view ----

    /// The split view when it shows now: the pair and the focused side.
    /// It shows when the active tab is one of the pair, outside page
    /// fullscreen, with the GPU chrome and one web view per tab.
    pub(super) fn split_layout(&self) -> Option<(Split, Side)> {
        let s = self.split?;
        if self.web_fullscreen || !self.gpu_chrome || self.shared_pane() || self.cheatsheet {
            return None;
        }
        let side = s.side_of(self.tab_id)?;
        let other = s.other(self.tab_id)?;
        self.tab_index(other)?;
        Some((s, side))
    }

    /// Shows tab `i` beside the active tab: the active one on the left, tab
    /// `i` on the right and focused. Tab `i` moves next to the active one
    /// when both are outside groups or in the same group.
    pub(super) fn split_with(&mut self, i: usize) {
        if i == self.active || i >= self.tabs.len() {
            self.set_notice("Pick another tab to show beside this one");
            return;
        }
        let (left, right) = (self.tab_id, self.tab_id_at(i));
        let Some(s) = Split::new(left, right) else { return };
        let same_group = self.groups.group_of(left).map(|g| g.id) == self.groups.group_of(right).map(|g| g.id);
        if same_group {
            let mut order = self.tab_order();
            order.retain(|t| *t != right);
            let at = order.iter().position(|t| *t == left).map_or(order.len(), |p| p + 1);
            order.insert(at, right);
            self.apply_order(&order);
        }
        self.split = Some(s);
        if let Some(j) = self.tab_index(right) {
            self.switch_tab(j);
        }
        self.request_redraw();
    }

    pub(super) fn close_split(&mut self) {
        if self.split.take().is_none() {
            self.set_notice("There is no split view");
        }
        self.request_redraw();
    }

    pub(super) fn swap_split(&mut self) {
        match self.split.as_mut() {
            Some(s) => s.swap(),
            None => self.set_notice("There is no split view"),
        }
        self.request_redraw();
    }

    /// Makes the other pane the focused one.
    pub(super) fn focus_other_pane(&mut self) {
        let Some(other) = self.split_layout().and_then(|(s, _)| s.other(self.tab_id)) else {
            self.set_notice("There is no split view");
            return;
        };
        if let Some(i) = self.tab_index(other) {
            self.switch_tab(i);
            if let Some(WebPane::Wry(wv)) = &self.webview {
                let _ = wv.focus();
            }
        }
    }

    /// A click in a page: in the other pane, that pane takes the focus.
    pub(super) fn page_clicked(&mut self, tab: u64) {
        if tab == self.tab_id || self.split_layout().is_none_or(|(s, _)| !s.contains(tab)) {
            return;
        }
        if let Some(i) = self.tab_index(tab) {
            self.switch_tab(i);
        }
    }

    /// The tab whose web view must never be unloaded: the visible partner.
    pub(super) fn split_partner(&self) -> Option<u64> {
        self.split_layout().and_then(|(s, _)| s.other(self.tab_id))
    }

    /// Whether the other pane needs the placeholder (no web page to show)
    /// and the title it shows.
    pub(super) fn peer_placeholder(&self) -> (bool, String) {
        let Some(i) = self.split_partner().and_then(|id| self.tab_index(id)) else {
            return (false, String::new());
        };
        let needs = self.tabs.get(i).is_none_or(|t| !t.web_page || !self.wry_ok);
        (needs, self.tab_title(i))
    }

    /// Creates and loads the web view of background tab `i` (the other
    /// pane of a split, unloaded).
    fn load_background_view(&mut self, i: usize) {
        let Some(window) = self.gfx.as_ref().map(|g| g.window.clone()) else { return };
        let Some((id, container, url)) = self.tabs.get(i).map(|t| (t.id, t.container.clone(), t.web_url.clone())) else {
            return;
        };
        if url.is_empty() || self.peer.load_tried == Some(id) {
            return;
        }
        self.peer.load_tried = Some(id);
        let Some(view) = self.create_webview(&window, id, container.as_deref()) else {
            self.set_notice("Could not create a web view for this tab");
            return;
        };
        let _ = view.load_url(&url);
        view.zoom(self.user_zoom as f64);
        if let Some(st) = self.tabs.get_mut(i) {
            st.view = Some(view);
            st.web_expect = true;
            st.web_committed = false;
            st.web_leaving.clear();
            st.web_document.clear();
            st.loading = true;
        }
    }

    /// Places (or hides) the other pane's web view; called with
    /// `sync_webview`. While a menu covers the page its picture is shown.
    pub(super) fn sync_split_peer(&mut self, menu_over_page: bool) {
        let want = self.split_partner().filter(|_| self.peer.rect.is_some());
        if let Some(old) = self.peer.maybe_hide.take()
            && Some(old) != want
            && old != self.tab_id
            && let Some(view) = self.tabs.iter().find(|t| t.id == old).and_then(|t| t.view.as_ref())
        {
            view.set_visible(false);
        }
        if self.peer.shown != want {
            if let Some(old) = self.peer.shown
                && old != self.tab_id
                && let Some(view) = self.tabs.iter().find(|t| t.id == old).and_then(|t| t.view.as_ref())
            {
                view.set_visible(false);
            }
            self.peer.shown = want;
            self.peer.visible = false;
            self.peer.bounds = None;
            self.peer.snapshot = None;
            self.peer.snapshot_pending = false;
        }
        let Some(peer) = want else { return };
        let Some(i) = self.tab_index(peer) else { return };
        if !self.tabs[i].web_page {
            return;
        }
        if self.tabs[i].view.is_none() && self.wry_ok {
            self.load_background_view(i);
        }
        let Some(rect) = self.peer.rect else { return };
        if menu_over_page && self.peer.visible && self.peer.snapshot.is_none() && !self.peer.snapshot_pending {
            #[cfg(target_os = "linux")]
            if let Some(WebPane::Wry(wv)) = &self.tabs[i].view {
                super::request_snapshot_gtk(wv, self.proxy.clone(), peer);
                self.peer.snapshot_pending = true;
            }
        }
        if !menu_over_page {
            self.peer.snapshot = None;
            self.peer.snapshot_pending = false;
        }
        let Some(view) = self.tabs[i].view.as_ref() else { return };
        let visible = !(self.egui_popup_open && view.is_native());
        let bounds = (rect.x.round() as i32, rect.y.round() as i32, rect.w.round().max(1.0) as u32, rect.h.round().max(1.0) as u32);
        if visible != self.peer.visible {
            view.set_visible(visible);
            self.peer.visible = visible;
            if visible && cfg!(target_os = "linux") {
                self.peer.bounds = None;
            }
        }
        if visible && self.peer.bounds != Some(bounds) {
            view.set_bounds(bounds);
            self.peer.bounds = Some(bounds);
        }
    }

    /// The other pane's picture arrived (`TabMessage::Snapshot`).
    pub(super) fn show_peer_snapshot(&mut self, tab: u64, w: usize, h: usize, rgba: Vec<u8>) {
        if self.peer.shown != Some(tab) || !self.peer.snapshot_pending {
            return;
        }
        self.peer.snapshot_pending = false;
        if w == 0 || h == 0 || rgba.len() != w * h * 4 || !self.egui_popup_open {
            return;
        }
        if let Some(egui) = &self.egui {
            let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            self.peer.snapshot = Some(egui.ctx().load_texture("browsecraft-split-snapshot", image, egui::TextureOptions::LINEAR));
        }
    }

    /// The pane positions the egui layout found (points, at `ppp`).
    pub(super) fn set_peer_rect(&mut self, peer: Option<egui::Rect>, ppp: f32) {
        self.peer.rect = peer.map(|r| Rect {
            x: (r.min.x * ppp).round(),
            y: (r.min.y * ppp).round(),
            w: (r.width() * ppp).round().max(1.0),
            h: (r.height() * ppp).round().max(1.0),
        });
    }

    /// Splits `full` (the whole page area, physical pixels) for the focused
    /// pane, when a split view shows.
    pub(super) fn focused_pane(&self, full: Rect) -> Rect {
        let Some((s, side)) = self.split_layout() else { return full };
        let scale = self.scale();
        let mark = (3.0 * scale).round();
        let (x, w) = s.pane(side, full.x, full.w, (split::DIVIDER * scale).round());
        Rect { x, y: full.y + mark, w, h: (full.h - mark).max(1.0) }
    }
}
