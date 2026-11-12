//! GPU-rendered browser chrome (toolbar, tab strip, Library sidebar) using `egui` on
//! `wgpu`, with the engine's software raster uploaded as a texture.
//!
//! This is the Phase 2 path. It is only constructed when the `chrome` flag
//! asks for it and a wgpu adapter/device can actually be created; otherwise
//! the browser falls back to the engine/softbuffer chrome in `browser.rs`.
//!
//! The engine keeps painting every frame into `Browser::frame`; here that
//! buffer is handed to egui as a texture and drawn inside the central panel,
//! while the toolbar and sidebar become real egui widgets.

use std::sync::Arc;

use winit::event::WindowEvent;
use winit::window::Window;

use crate::browser::ContainerChoice;
use crate::containers::Container;
use crate::flags::{self, Chrome};
use crate::split::Side;
use crate::tab_groups::{GroupColor, StripItem};

/// The GPU-rendered chrome. Holds the egui context, the winit bridge and the
/// wgpu painter. `Browser` owns one of these when the egui path is active.
pub struct EguiChrome {
    ctx: egui::Context,
    state: egui_winit::State,
    painter: egui_wgpu::winit::Painter,
    viewport_id: egui::ViewportId,
}

impl EguiChrome {
    /// Tries to create the GPU chrome for `window`. Returns `None` when no
    /// adapter/device is available so the caller can keep the engine chrome.
    pub fn new(window: Arc<Window>) -> Option<Self> {
        let ctx = egui::Context::default();
        install_icon_font(&ctx);
        let viewport_id = egui::ViewportId::ROOT;
        let mut state = egui_winit::State::new(
            ctx.clone(),
            viewport_id,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let mut painter = pollster::block_on(egui_wgpu::winit::Painter::new(
            ctx.clone(),
            egui_wgpu::WgpuConfiguration::default(),
            false,
            egui_wgpu::RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(viewport_id, Some(window.clone()))).ok()?;
        painter.render_state()?;
        if let Some(max) = painter.max_texture_side() {
            state.set_max_texture_side(max);
        }
        Some(EguiChrome {
            ctx,
            state,
            painter,
            viewport_id,
        })
    }

    /// Feeds a winit event into egui. Returns `true` when egui consumed it.
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        // egui-winit asks for a repaint on `RedrawRequested` itself (it
        // means "paint now", which the caller is about to do): asking for
        // another one here would redraw every frame forever. Animations
        // ask through `frame`'s repaint delay instead.
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            window.request_redraw();
        }
        response.consumed
    }

    /// The largest texture side the GPU accepts. A bigger texture fails wgpu
    /// validation, which panics.
    pub fn max_texture_side(&self) -> usize {
        self.painter.max_texture_side().unwrap_or(2048)
    }

    pub fn on_resized(&mut self, w: u32, h: u32) {
        if let (Some(w), Some(h)) = (std::num::NonZeroU32::new(w), std::num::NonZeroU32::new(h)) {
            self.painter.on_window_resized(self.viewport_id, w, h);
        }
    }

    /// Runs one egui frame and presents it. `build` draws the chrome; it may
    /// mutate browser state directly. The engine raster `frame`
    /// (`w × h` physical pixels, `0x00RRGGBB`) is uploaded as the page texture
    /// shown in the central panel (when `show_page` is true).
    ///
    /// Returns when egui wants the next frame (animations, tooltips), or
    /// `None` when nothing changes until the next input.
    pub fn frame<R>(&mut self, window: &Arc<Window>, raw_input: egui::RawInput, build: R) -> Option<std::time::Duration>
    where
        R: FnMut(&mut egui::Ui),
    {
        let mut full_output = self.ctx.run_ui(raw_input, build);
        let repaint = full_output
            .viewport_output
            .get(&self.viewport_id)
            .map(|v| v.repaint_delay)
            .filter(|d| *d < std::time::Duration::from_secs(3600));
        let ppp = self.ctx.pixels_per_point();
        self.state
            .handle_platform_output(window, full_output.platform_output.clone());
        let paint_jobs = self
            .ctx
            .tessellate(std::mem::take(&mut full_output.shapes), ppp);
        let _ = self.painter.paint_and_update_textures(
            self.viewport_id,
            ppp,
            [0.0, 0.0, 0.0, 0.0],
            &paint_jobs,
            &mut full_output.textures_delta,
            Vec::new(),
            window,
        );
        repaint
    }

    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    pub fn take_raw_input(&mut self, window: &Window) -> egui::RawInput {
        self.state.take_egui_input(window)
    }
}

/// The central panel is only a painted page, but egui's `consumed` flag
/// includes its background. Inside that page, only overlays or a drag that
/// started on a chrome widget should keep pointer input from the page engine.
pub fn blocks_page_pointer(ctx: &egui::Context, position: egui::Pos2) -> bool {
    egui::Popup::is_any_open(ctx)
        || ctx.egui_is_using_pointer()
        || ctx.layer_id_at(position) != Some(egui::LayerId::background())
}

/// Which chrome the flags ask for, given whether the GPU path came up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Engine,
    Egui,
}

/// Resolves the `chrome` flag: `engine` always uses the engine path, while
/// `auto`/`egui` use the GPU path when it successfully initialised.
pub fn resolve(gpu_available: bool) -> Choice {
    match flags::get().chrome {
        Chrome::Engine => Choice::Engine,
        Chrome::Egui | Chrome::Auto if gpu_available => Choice::Egui,
        Chrome::Egui | Chrome::Auto => Choice::Engine,
    }
}

// ---- Icons and theming -----------------------------------------------------

// Phosphor codepoints used by the chrome (see `crate::icons`). Re-exported
// under the historical `ICON_*` names for the call sites in `browser.rs`.
pub use crate::icons::{
    BACK as ICON_BACK, BOOKMARKS as ICON_BOOKMARKS, CLOSE as ICON_STOP, CLOSE as ICON_XMARK,
    FORWARD as ICON_FORWARD, GEAR as ICON_GEAR, GLOBE as ICON_GLOBE, HOME as ICON_HOME,
    KEY as ICON_KEY, LOCK as ICON_LOCK, PLUS as ICON_PLUS, RELOAD as ICON_RELOAD,
    STAR as ICON_STAR,
};

/// Icon text at `size` points, in the Phosphor family.
pub fn icon(code: &str, size: f32) -> egui::RichText {
    egui::RichText::new(code)
        .family(crate::icons::family())
        .size(size)
}

/// A compact square icon button that shows its label on hover. Uses the theme
/// visuals for hover/press feedback.
pub fn icon_button(
    ui: &mut egui::Ui,
    enabled: bool,
    code: &str,
    tooltip: &str,
    size: f32,
) -> egui::Response {
    let button = egui::Button::new(icon(code, size))
        .min_size(egui::vec2(32.0, 32.0))
        .corner_radius(egui::CornerRadius::same(10));
    let response = ui.add_enabled(enabled, button);
    if enabled {
        response.on_hover_text(tooltip)
    } else {
        response
    }
}

/// Stable id of the address-bar text field, so focus can be requested from
/// keyboard shortcuts elsewhere in the frame.
pub fn address_id() -> egui::Id {
    egui::Id::new("bc-address")
}

/// The rounded address field: a leading scheme icon, the text, and (for pages
/// that can be bookmarked) a star on the right. `star` is `Some(is_starred)`
/// or `None` when the page cannot be bookmarked. Returns the text edit's
/// response and whether the star was clicked.
/// What the user clicked in the address bar besides the field.
#[derive(Default)]
pub struct AddressClicks {
    pub star: bool,
    /// The zoom badge (back to 100%).
    pub zoom: bool,
}

/// The address field with the page's icon, the zoom badge (`zoom`: the
/// percentage when it is not 100, as Firefox shows it) and the bookmark star.
pub fn address_bar(
    ui: &mut egui::Ui,
    text: &mut String,
    width: f32,
    c: &Colors,
    leading: &str,
    star: Option<bool>,
    zoom: Option<u32>,
) -> (egui::Response, AddressClicks) {
    let focused = ui.memory(|m| m.has_focus(address_id()));
    let stroke = if focused {
        egui::Stroke::new(1.5, c.accent)
    } else {
        egui::Stroke::new(1.0, c.line)
    };
    let mut clicks = AddressClicks::default();
    let zoom_label = zoom.map(|z| format!("{z}%"));
    let response = egui::Frame::NONE
        .fill(c.field_bg)
        .stroke(stroke)
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(12, 3))
        .show(ui, |ui| {
            ui.set_width((width - 24.0).max(40.0));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(icon(leading, 13.0).color(c.muted));
                let zoom_w = zoom_label.as_ref().map_or(0.0, |l| l.chars().count() as f32 * 7.5 + 22.0);
                let star_w = if star.is_some() { 26.0 } else { 0.0 } + zoom_w;
                let response = ui.add_sized(
                    [(ui.available_width() - star_w).max(20.0), 24.0],
                    egui::TextEdit::singleline(text)
                        .id(address_id())
                        .frame(egui::Frame::NONE)
                        .text_color(c.text)
                        .hint_text(egui::RichText::new("Search or enter address").color(c.muted)),
                );
                if let Some(label) = &zoom_label {
                    let badge = egui::Button::new(egui::RichText::new(label).size(12.0).color(c.text))
                        .fill(c.hover)
                        .stroke(egui::Stroke::new(1.0, c.line))
                        .corner_radius(egui::CornerRadius::same(10))
                        .min_size(egui::vec2(0.0, 20.0));
                    clicks.zoom = ui.add(badge).on_hover_text("Current zoom. Click to reset to 100% (Ctrl+0)").clicked();
                }
                if let Some(on) = star {
                    let color = if on { c.accent } else { c.muted };
                    let tip = if on { "Remove bookmark (Ctrl+D)" } else { "Bookmark this page (Ctrl+D)" };
                    let button = egui::Button::new(icon(ICON_STAR, 15.0).color(color))
                        .frame(false)
                        .min_size(egui::vec2(24.0, 24.0));
                    clicks.star = ui.add(button).on_hover_text(tip).clicked();
                }
                response
            })
            .inner
        })
        .inner;
    (response, clicks)
}

/// A theme's swatch: its background with its surface and accent as dots.
pub fn theme_swatch(ui: &mut egui::Ui, theme: &crate::theme::Theme, size: egui::Vec2) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter();
    let t = &theme.t;
    p.rect_filled(rect, egui::CornerRadius::same(5), rgb(t.background));
    p.rect_stroke(rect, egui::CornerRadius::same(5), egui::Stroke::new(1.0, rgb(t.border)), egui::StrokeKind::Inside);
    let r = (rect.height() * 0.22).max(2.0);
    let y = rect.center().y;
    p.circle_filled(egui::pos2(rect.min.x + rect.width() * 0.33, y), r, rgb(t.surface2));
    p.circle_filled(egui::pos2(rect.min.x + rect.width() * 0.67, y), r, rgb(t.accent));
    rect
}

/// One row of a theme list: its swatch, its name and a check on the current
/// one. "Follow system" shows the light and dark halves.
pub fn theme_item(ui: &mut egui::Ui, c: &Colors, id: crate::theme::ThemeId, current: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), 28.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(6), c.hover);
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(8.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 8.0;
    match id {
        crate::theme::ThemeId::System => {
            let swatch = egui::vec2(15.0, 18.0);
            child.spacing_mut().item_spacing.x = 0.0;
            theme_swatch(&mut child, &crate::theme::LIGHT, swatch);
            theme_swatch(&mut child, &crate::theme::DARK, swatch);
            child.add_space(8.0);
        }
        other => {
            theme_swatch(&mut child, crate::theme::get(other), egui::vec2(30.0, 18.0));
        }
    }
    let weight = egui::RichText::new(id.label()).size(13.0).color(c.text);
    child.label(if current { weight.strong() } else { weight });
    if current {
        child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(icon(crate::icons::CHECK, 13.0).color(c.accent));
        });
    }
    resp
}

/// The theme button of the toolbar and its menu. Returns the theme picked.
pub fn theme_button(ui: &mut egui::Ui, c: &Colors, current: crate::theme::ThemeId) -> Option<crate::theme::ThemeId> {
    let button = icon_button(ui, true, crate::icons::THEME, "Theme", 16.0);
    let mut pick = None;
    egui::Popup::menu(&button).align(egui::RectAlign::BOTTOM_END).width(230.0).show(|ui| {
        ui.label(egui::RichText::new("Theme").size(12.0).color(c.muted));
        for id in crate::theme::ThemeId::ALL {
            if theme_item(ui, c, id, id == current).clicked() {
                pick = Some(id);
                ui.close();
            }
        }
    });
    pick
}

/// What the user did in the bookmarks bar.
pub enum BarEvent {
    Open(String),
    OpenInNewTab(String),
    /// Remove the bookmark or folder with this id.
    Remove(u64),
    Manage,
    /// Open the bookmarks page on this folder.
    ManageFolder(u64),
}

/// The debug window script opens this bar folder's menu (its id; 0: none).
#[cfg(debug_assertions)]
pub static OPEN_BAR_FOLDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Entries listed in one folder menu of the bar; the rest are on the
/// bookmarks page.
const FOLDER_MENU_MAX: usize = 300;

/// The contents of a bookmarks-bar folder as a menu, subfolders as
/// submenus.
fn folder_menu(ui: &mut egui::Ui, c: &Colors, folder: &crate::bookmarks::Node, events: &mut Vec<BarEvent>) {
    if folder.children.is_empty() {
        ui.label(egui::RichText::new("(empty)").size(12.5).color(c.muted));
    }
    for child in folder.children.iter().take(FOLDER_MENU_MAX) {
        let label = egui::RichText::new(format!(
            "{}  {}",
            if child.is_folder() { crate::icons::FOLDER } else { crate::icons::GLOBE },
            child.title
        ))
        .size(13.0)
        .color(c.text);
        match &child.url {
            None => {
                ui.menu_button(label, |ui| folder_menu(ui, c, child, events));
            }
            Some(url) => {
                let resp = ui
                    .add(egui::Button::new(label).frame(false).truncate().min_size(egui::vec2(220.0, 24.0)))
                    .on_hover_text(url);
                if resp.clicked() {
                    events.push(BarEvent::Open(url.clone()));
                    ui.close();
                } else if resp.middle_clicked() {
                    events.push(BarEvent::OpenInNewTab(url.clone()));
                    ui.close();
                }
            }
        }
    }
    ui.separator();
    let more = folder.children.len().saturating_sub(FOLDER_MENU_MAX);
    let text = if more > 0 { format!("{more} more in the bookmarks manager…") } else { "Open in the bookmarks manager".into() };
    if menu_item(ui, c, crate::icons::BOOKMARKS, &text).clicked() {
        events.push(BarEvent::ManageFolder(folder.id));
        ui.close();
    }
}

/// A small icon-and-label entry for popup menus.
/// A submenu listing the session presets (`crate::presets`); returns the
/// one clicked. Nothing is shown when there are none.
pub fn presets_submenu(ui: &mut egui::Ui, c: &Colors, label: &str) -> Option<u64> {
    let list = crate::presets::summaries();
    if list.is_empty() {
        return None;
    }
    let mut picked = None;
    ui.menu_button(egui::RichText::new(format!("{}  {label}", egui_phosphor::regular::STACK)).size(13.0).color(c.text), |ui| {
        for (id, name, summary) in &list {
            let name = crate::browser::display_text(name, 40);
            if menu_item(ui, c, egui_phosphor::regular::BROWSERS, &name).on_hover_text(summary).clicked() {
                picked = Some(*id);
                ui.close();
            }
        }
    });
    picked
}

pub fn menu_item(ui: &mut egui::Ui, c: &Colors, code: &str, label: &str) -> egui::Response {
    let text = egui::RichText::new(format!("{code}  {label}")).size(13.0).color(c.text);
    ui.add(egui::Button::new(text).frame(false).min_size(egui::vec2(180.0, 26.0)))
}

/// The bookmarks bar contents: one chip per bookmark that fits, an overflow
/// menu for the rest, and an "All bookmarks" button on the right.
pub fn bookmarks_bar(ui: &mut egui::Ui, c: &Colors, items: &[crate::bookmarks::Node]) -> Vec<BarEvent> {
    const CHIP_MAX_TEXT: f32 = 150.0;
    const CHIP_PAD: f32 = 10.0;
    const ICON_W: f32 = 14.0;
    const GAP: f32 = 4.0;
    let mut events = Vec::new();
    let font = egui::FontId::proportional(13.0);
    let reserve = 128.0; // the "All bookmarks" button
    let budget = (ui.available_width() - reserve - 8.0).max(0.0);

    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        ui.add_space(8.0);
        let mut used = 0.0;
        let mut overflow: Vec<&crate::bookmarks::Node> = Vec::new();
        for item in items {
            let text_w = ui
                .painter()
                .layout_no_wrap(item.title.clone(), font.clone(), c.text)
                .size()
                .x
                .min(CHIP_MAX_TEXT);
            let w = CHIP_PAD * 2.0 + ICON_W + 6.0 + text_w;
            if !(overflow.is_empty() && used + w + GAP <= budget - 30.0) {
                overflow.push(item);
                continue;
            }
            used += w + GAP;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, egui::CornerRadius::same(13), c.hover);
            }
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect.shrink2(egui::vec2(CHIP_PAD, 0.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.spacing_mut().item_spacing.x = 6.0;
            let glyph = if item.is_folder() { crate::icons::FOLDER } else { crate::icons::GLOBE };
            child.label(icon(glyph, 12.0).color(c.accent));
            child.add(
                egui::Label::new(egui::RichText::new(&item.title).size(13.0).color(c.text))
                    .truncate()
                    .selectable(false),
            );
            match &item.url {
                None => {
                    // A folder opens as a menu under its chip.
                    #[cfg(debug_assertions)]
                    if OPEN_BAR_FOLDER
                        .compare_exchange(item.id, 0, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed)
                        .is_ok()
                    {
                        egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&resp));
                    }
                    egui::Popup::menu(&resp).show(|ui| folder_menu(ui, c, item, &mut events));
                    resp.context_menu(|ui| {
                        if menu_item(ui, c, crate::icons::BOOKMARKS, "Open in the bookmarks manager").clicked() {
                            events.push(BarEvent::ManageFolder(item.id));
                            ui.close();
                        }
                        if menu_item(ui, c, crate::icons::TRASH, "Delete folder").clicked() {
                            events.push(BarEvent::Remove(item.id));
                            ui.close();
                        }
                    });
                }
                Some(url) => {
                    let resp = resp.on_hover_text(url);
                    if resp.clicked() {
                        events.push(BarEvent::Open(url.clone()));
                    } else if resp.middle_clicked() {
                        events.push(BarEvent::OpenInNewTab(url.clone()));
                    }
                    resp.context_menu(|ui| {
                        if menu_item(ui, c, crate::icons::GLOBE, "Open").clicked() {
                            events.push(BarEvent::Open(url.clone()));
                            ui.close();
                        }
                        if menu_item(ui, c, crate::icons::PLUS, "Open in new tab").clicked() {
                            events.push(BarEvent::OpenInNewTab(url.clone()));
                            ui.close();
                        }
                        if menu_item(ui, c, crate::icons::TRASH, "Remove bookmark").clicked() {
                            events.push(BarEvent::Remove(item.id));
                            ui.close();
                        }
                    });
                }
            }
        }
        if items.is_empty() {
            ui.label(
                egui::RichText::new("Bookmarks you add show up here. Click the star in the address bar.")
                    .size(12.5)
                    .color(c.muted),
            );
        }
        if !overflow.is_empty() {
            ui.menu_button(egui::RichText::new("\u{bb}").size(15.0).color(c.text), |ui| {
                let rest = crate::bookmarks::Node {
                    id: crate::bookmarks::BAR_ID,
                    title: String::new(),
                    url: None,
                    children: overflow.into_iter().cloned().collect(),
                    added: 0,
                    import: 0,
                };
                folder_menu(ui, c, &rest, &mut events);
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            let label = egui::RichText::new(format!("{}  All bookmarks", ICON_BOOKMARKS))
                .size(12.5)
                .color(c.muted);
            let button = egui::Button::new(label)
                .frame(false)
                .corner_radius(egui::CornerRadius::same(13))
                .min_size(egui::vec2(0.0, 26.0));
            if ui.add(button).on_hover_text("Open the bookmarks page (Ctrl+Shift+O)").clicked() {
                events.push(BarEvent::Manage);
            }
        });
    });
    events
}

/// Selects the whole address field (used when the address bar is focused by
/// shortcut, matching the engine chrome's select-all behaviour).
pub fn select_all(ctx: &egui::Context, id: egui::Id, chars: usize) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        let range = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0usize),
            egui::text::CCursor::new(chars),
        );
        state.cursor.set_char_range(Some(range));
        state.store(ctx, id);
    }
}

/// What a tab shows in the strip or the sidebar.
pub struct TabInfo<'a> {
    pub title: &'a str,
    pub active: bool,
    pub container: Option<&'a Container>,
    pub favicon: Option<egui::TextureId>,
    /// Unloaded to save memory: drawn dimmed, it reloads when shown.
    pub unloaded: bool,
    /// The tab's group (id) and its colour: an underline in the strip, a
    /// bar and an indent in the sidebar.
    pub group: Option<(u64, egui::Color32)>,
    /// The tab is one of the split view's pair (which pane).
    pub split: Option<Side>,
    /// A split view exists and this tab is the focused pane's partner
    /// candidate: "Open in split view with current tab" is offered.
    pub can_split: bool,
    /// Show the title in a tooltip (off when hover cards show it).
    pub tooltip: bool,
}

/// The other browser windows, for "Move to window": (id, label).
pub type WindowList<'a> = &'a [(u64, String)];

/// A tab group as the strip and the sidebar draw it.
pub struct GroupInfo<'a> {
    pub id: u64,
    pub name: &'a str,
    pub color: GroupColor,
    pub collapsed: bool,
    /// How many tabs it has (shown on a collapsed chip).
    pub tabs: usize,
    /// Open the editor (name and colour) now: the group was just made.
    pub edit: bool,
}

/// What the user did to a group (its chip, header row or editor).
#[derive(Clone, Debug, PartialEq)]
pub enum GroupEvent {
    ToggleCollapsed,
    Rename(String),
    Color(GroupColor),
    NewTab,
    Ungroup,
    Close,
}

/// What the user did to the split view from a tab's menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitEvent {
    /// Pair this tab with the active one.
    WithActive,
    Swap,
    Close,
}

/// Outcome of drawing one tab (strip chip or sidebar row).
#[derive(Default)]
pub struct TabRow {
    pub clicked: bool,
    pub close: bool,
    /// The user picked "Duplicate tab" from the context menu.
    pub duplicate: bool,
    /// The user picked "Close other tabs" from the context menu.
    pub close_others: bool,
    /// "Open page in another browser" (the system's default one).
    pub open_externally: bool,
    /// "New tab in container" from the context menu.
    pub new_in: Option<ContainerChoice>,
    /// "Reopen in container": this tab's page in another container.
    pub reopen_in: Option<ContainerChoice>,
    /// "Add tab to new group".
    pub new_group: bool,
    /// "Add tab to group" with an existing group.
    pub add_to_group: Option<u64>,
    /// "Remove from group".
    pub remove_from_group: bool,
    pub split: Option<SplitEvent>,
    /// "Move to new window".
    pub move_to_new_window: bool,
    /// "Move to window ▸ <window>".
    pub move_to_window: Option<u64>,
    /// The pointer rests on it, and where it is (points).
    pub hovered: bool,
    pub rect: Option<egui::Rect>,
    /// A drag of the chip started (the strip only).
    pub drag_started: bool,
}

/// The menu of containers: no container, each container, a new temporary
/// one. Returns the pick.
pub fn container_menu(ui: &mut egui::Ui, c: &Colors, containers: &[Container], none_label: &str) -> Option<ContainerChoice> {
    let mut pick = None;
    if menu_item(ui, c, ICON_GLOBE, none_label).clicked() {
        pick = Some(ContainerChoice::None);
    }
    for ctr in containers {
        let text = egui::RichText::new(format!("{}  {}", ctr.glyph(), ctr.name)).size(13.0).color(ctr.color32());
        if ui.add(egui::Button::new(text).frame(false).min_size(egui::vec2(180.0, 26.0))).clicked() {
            pick = Some(ContainerChoice::Id(ctr.id.clone()));
        }
    }
    let temp = egui::RichText::new(format!("{}  New temporary container", egui_phosphor::regular::TIMER))
        .size(13.0)
        .color(c.text);
    if ui
        .add(egui::Button::new(temp).frame(false).min_size(egui::vec2(180.0, 26.0)))
        .on_hover_text("Its cookies and site data are deleted when its last tab closes (Ctrl+Shift+N)")
        .clicked()
    {
        pick = Some(ContainerChoice::Temporary);
    }
    if pick.is_some() {
        ui.close();
    }
    pick
}

/// The right-click menu of a tab.
fn tab_menu(ui: &mut egui::Ui, c: &Colors, tab: &TabInfo, containers: &[Container], groups: &[GroupInfo], windows: WindowList, out: &mut TabRow) {
    if menu_item(ui, c, crate::icons::COPY, "Duplicate tab").clicked() {
        out.duplicate = true;
        ui.close();
    }
    if menu_item(ui, c, egui_phosphor::regular::COMPASS, "Open page in another browser").clicked() {
        out.open_externally = true;
        ui.close();
    }
    if menu_item(ui, c, egui_phosphor::regular::ARROW_SQUARE_OUT, "Move to new window").clicked() {
        out.move_to_new_window = true;
        ui.close();
    }
    if !windows.is_empty() {
        ui.menu_button(
            egui::RichText::new(format!("{}  Move to window", egui_phosphor::regular::APP_WINDOW)).size(13.0).color(c.text),
            |ui| {
                for (key, label) in windows {
                    if menu_item(ui, c, egui_phosphor::regular::APP_WINDOW, label).clicked() {
                        out.move_to_window = Some(*key);
                        ui.close();
                    }
                }
            },
        );
    }
    let current = tab.group.map(|g| g.0);
    let others: Vec<&GroupInfo> = groups.iter().filter(|g| Some(g.id) != current).collect();
    if others.is_empty() {
        if menu_item(ui, c, egui_phosphor::regular::FOLDER_SIMPLE_PLUS, "Add tab to new group").clicked() {
            out.new_group = true;
            ui.close();
        }
    } else {
        ui.menu_button(
            egui::RichText::new(format!("{}  Add tab to group", egui_phosphor::regular::FOLDER_SIMPLE_PLUS)).size(13.0).color(c.text),
            |ui| {
                if menu_item(ui, c, ICON_PLUS, "New group").clicked() {
                    out.new_group = true;
                    ui.close();
                }
                ui.separator();
                for g in others {
                    let name = if g.name.is_empty() { "Unnamed group" } else { g.name };
                    let text = egui::RichText::new(format!("{}  {name}", egui_phosphor::regular::CIRCLE)).size(13.0).color(c.group_color(g.color));
                    if ui.add(egui::Button::new(text).frame(false).min_size(egui::vec2(180.0, 26.0))).clicked() {
                        out.add_to_group = Some(g.id);
                        ui.close();
                    }
                }
            },
        );
    }
    if current.is_some() && menu_item(ui, c, egui_phosphor::regular::FOLDER_SIMPLE_MINUS, "Remove from group").clicked() {
        out.remove_from_group = true;
        ui.close();
    }
    if tab.split.is_some() {
        if menu_item(ui, c, egui_phosphor::regular::ARROWS_LEFT_RIGHT, "Swap split panes").clicked() {
            out.split = Some(SplitEvent::Swap);
            ui.close();
        }
        if menu_item(ui, c, egui_phosphor::regular::SQUARE_SPLIT_HORIZONTAL, "Close split view").clicked() {
            out.split = Some(SplitEvent::Close);
            ui.close();
        }
    } else if tab.can_split
        && menu_item(ui, c, egui_phosphor::regular::COLUMNS, "Open in split view with current tab").clicked()
    {
        out.split = Some(SplitEvent::WithActive);
        ui.close();
    }
    ui.separator();
    ui.menu_button(
        egui::RichText::new(format!("{}  New tab in container", egui_phosphor::regular::STACK)).size(13.0).color(c.text),
        |ui| {
            out.new_in = container_menu(ui, c, containers, "No container");
        },
    );
    ui.menu_button(
        egui::RichText::new(format!("{}  Reopen in container", egui_phosphor::regular::ARROW_SQUARE_OUT)).size(13.0).color(c.text),
        |ui| {
            out.reopen_in = container_menu(ui, c, containers, "No container");
        },
    );
    ui.separator();
    if menu_item(ui, c, crate::icons::CLOSE, "Close tab").clicked() {
        out.close = true;
        ui.close();
    }
    if menu_item(ui, c, crate::icons::TRASH, "Close other tabs").clicked() {
        out.close_others = true;
        ui.close();
    }
}

/// The editor of a group (Chrome's bubble): name, colour and actions.
fn group_editor(ui: &mut egui::Ui, c: &Colors, group: &GroupInfo, out: &mut Vec<GroupEvent>) {
    ui.set_min_width(230.0);
    let buffer_id = egui::Id::new(("bc-group-name", group.id));
    let mut name = ui.data_mut(|d| d.get_temp::<String>(buffer_id)).unwrap_or_else(|| group.name.to_string());
    let edit = ui.add(
        egui::TextEdit::singleline(&mut name)
            .id(egui::Id::new(("bc-group-name-field", group.id)))
            .hint_text(egui::RichText::new("Name this group").color(c.muted))
            .desired_width(f32::INFINITY),
    );
    if group.edit {
        edit.request_focus();
    }
    if edit.changed() {
        out.push(GroupEvent::Rename(name.clone()));
    }
    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        ui.close();
    }
    ui.data_mut(|d| d.insert_temp(buffer_id, name));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for color in GroupColor::ALL {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            let fill = c.group_color(color);
            ui.painter().circle_filled(rect.center(), 8.0, fill);
            if color == group.color {
                ui.painter().circle_stroke(rect.center(), 9.5, egui::Stroke::new(1.5, c.text));
            }
            if resp.on_hover_text(color.name()).clicked() {
                out.push(GroupEvent::Color(color));
            }
        }
    });
    ui.separator();
    if menu_item(ui, c, ICON_PLUS, "New tab in group").clicked() {
        out.push(GroupEvent::NewTab);
        ui.close();
    }
    let (icon_code, label) = if group.collapsed {
        (egui_phosphor::regular::CARET_DOWN, "Expand group")
    } else {
        (egui_phosphor::regular::CARET_RIGHT, "Collapse group")
    };
    if menu_item(ui, c, icon_code, label).clicked() {
        out.push(GroupEvent::ToggleCollapsed);
        ui.close();
    }
    if menu_item(ui, c, egui_phosphor::regular::FOLDER_SIMPLE_MINUS, "Ungroup").clicked() {
        out.push(GroupEvent::Ungroup);
        ui.close();
    }
    if menu_item(ui, c, crate::icons::TRASH, "Close group").clicked() {
        out.push(GroupEvent::Close);
        ui.close();
    }
}

/// Attaches the group editor to a chip or header row: it opens on a
/// right-click, and at once for a group just made.
fn group_editor_popup(ui: &mut egui::Ui, c: &Colors, resp: &egui::Response, group: &GroupInfo) -> Vec<GroupEvent> {
    let mut out = Vec::new();
    let open = group.edit || resp.secondary_clicked();
    if open {
        // A fresh editor starts from the group's name.
        ui.data_mut(|d| d.remove::<String>(egui::Id::new(("bc-group-name", group.id))));
    }
    egui::Popup::from_response(resp)
        .id(egui::Id::new(("bc-group-editor", group.id)))
        .kind(egui::PopupKind::Menu)
        .open_memory(open.then_some(egui::SetOpenCommand::Bool(true)))
        .align(egui::RectAlign::BOTTOM_START)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .width(240.0)
        .show(|ui| group_editor(ui, c, group, &mut out));
    out
}

/// The text drawn on a group's coloured chip: readable on the group colour
/// in both themes.
fn group_label_color(c: &Colors) -> egui::Color32 {
    if c.is_dark() { c.sidebar_bg } else { c.field_bg }
}

/// A group's chip in the strip: its name on its colour (a dot when it has
/// none, the tab count when collapsed). Click: collapse or expand;
/// right-click: the editor.
/// A group chip's text: its name, and the tab count when collapsed.
fn group_chip_text(group: &GroupInfo) -> String {
    let mut text = group.name.to_string();
    if group.collapsed {
        if !text.is_empty() {
            text.push_str("  ");
        }
        text.push_str(&group.tabs.to_string());
    }
    text
}

/// The width of a group chip in the strip.
fn group_chip_width(ui: &egui::Ui, group: &GroupInfo) -> f32 {
    let text = group_chip_text(group);
    if text.is_empty() {
        return 22.0;
    }
    let w = ui.fonts_mut(|f| f.layout_no_wrap(text, egui::FontId::proportional(12.0), egui::Color32::WHITE).size().x);
    w.min(140.0) + 20.0
}

fn group_chip(ui: &mut egui::Ui, c: &Colors, group: &GroupInfo) -> (egui::Rect, Vec<GroupEvent>) {
    let fill = c.group_color(group.color);
    let text = group_chip_text(group);
    let font = egui::FontId::proportional(12.0);
    let width = group_chip_width(ui, group);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click());
    let pill = egui::Rect::from_center_size(rect.center(), egui::vec2(width - 4.0, 20.0));
    let painter = ui.painter();
    if text.is_empty() {
        painter.circle_filled(rect.center(), 6.0, fill);
    } else {
        painter.rect_filled(pill, egui::CornerRadius::same(6), fill);
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(text, font, group_label_color(c)));
        let clip = painter.with_clip_rect(pill.shrink2(egui::vec2(6.0, 0.0)));
        clip.galley(egui::pos2(pill.min.x + 8.0, pill.center().y - galley.size().y / 2.0), galley, group_label_color(c));
    }
    if resp.hovered() {
        painter.rect_stroke(pill.expand(1.5), egui::CornerRadius::same(7), egui::Stroke::new(1.0, fill), egui::StrokeKind::Outside);
    }
    // The group's underline starts under its chip.
    if !group.collapsed {
        let line = egui::Rect::from_min_max(egui::pos2(rect.center().x, rect.max.y - 2.0), egui::pos2(rect.max.x + 2.0, rect.max.y));
        painter.rect_filled(line, 0.0, fill);
    }
    let tip = match (group.name.is_empty(), group.collapsed) {
        (true, true) => "Expand group".to_string(),
        (true, false) => "Collapse group".to_string(),
        (false, true) => format!("{}: expand ({} tabs)", group.name, group.tabs),
        (false, false) => format!("{}: collapse", group.name),
    };
    let resp = resp.on_hover_text(format!("{tip}\nRight-click to edit"));
    let mut out = group_editor_popup(ui, c, &resp, group);
    if resp.clicked() {
        out.push(GroupEvent::ToggleCollapsed);
    }
    (rect, out)
}

/// The page icon of a tab: its favicon, or a globe.
fn tab_icon(ui: &mut egui::Ui, c: &Colors, tab: &TabInfo) {
    match tab.favicon {
        Some(texture) => {
            let image = egui::Image::new((texture, egui::vec2(16.0, 16.0)));
            ui.add(if tab.unloaded { image.tint(egui::Color32::from_white_alpha(140)) } else { image });
        }
        None => {
            let color = match tab.container {
                Some(ctr) => ctr.color32(),
                None if tab.active => c.accent,
                None => c.muted,
            };
            ui.label(icon(ICON_GLOBE, 13.0).color(color));
        }
    }
}

/// What the user did in the tab strip.
#[derive(Default)]
pub struct TabStrip {
    /// Per tab (by index), what happened to it.
    pub tabs: Vec<(usize, TabRow)>,
    /// Per group (by id), what happened to it.
    pub groups: Vec<(u64, GroupEvent)>,
    /// "+" (no container) or a pick from the container menu.
    pub new_tab: Option<ContainerChoice>,
    /// "Manage containers" in the container menu.
    pub manage_containers: bool,
    /// A session preset to open in a new window (the same menu).
    pub open_preset: Option<u64>,
    /// Each visible chip: (tab index, rectangle), in strip order.
    pub chips: Vec<(usize, egui::Rect)>,
    /// The "Show all tabs" button.
    pub overview: bool,
}

/// The width of each tab in a strip `available` points wide: as wide as
/// Chrome's (up to 220), shrinking as tabs are added, never below 48 (the
/// strip then scrolls).
pub fn tab_width(available: f32, count: usize) -> f32 {
    if count == 0 {
        return 220.0;
    }
    (available / count as f32 - 2.0).clamp(48.0, 220.0)
}

/// The horizontal tab strip (tabs on top): `items` in order, a chip per
/// group (coloured, with an underline along its tabs) and per visible tab
/// (icon, title, close button, container colour); then "+" and the
/// container picker. The split view's two tabs share an outline.
#[allow(clippy::too_many_arguments)]
pub fn tab_strip(
    ui: &mut egui::Ui,
    c: &Colors,
    tabs: &[TabInfo],
    items: &[StripItem],
    groups: &[GroupInfo],
    containers: &[Container],
    windows: WindowList,
    drop_marker: Option<usize>,
) -> TabStrip {
    let mut out = TabStrip::default();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(6.0);
        let buttons_w = 106.0;
        let chips_w: f32 = items
            .iter()
            .filter_map(|i| match i {
                StripItem::Group(id) => groups.iter().find(|g| g.id == *id),
                StripItem::Tab(_) => None,
            })
            .map(|g| group_chip_width(ui, g) + 2.0)
            .sum();
        let available = (ui.available_width() - buttons_w).max(48.0);
        let shown = items.iter().filter(|i| matches!(i, StripItem::Tab(_))).count();
        let width = tab_width((available - chips_w).max(48.0), shown);
        // Wide enough for every chip when they fit, so the strip never
        // scrolls a tab out of sight for nothing.
        let content_w = chips_w + shown as f32 * (width + 2.0);
        egui::ScrollArea::horizontal()
            .max_width(available)
            .min_scrolled_width(content_w.min(available))
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut split_rects: Vec<egui::Rect> = Vec::new();
                    for item in items {
                        match *item {
                            StripItem::Group(id) => {
                                if let Some(group) = groups.iter().find(|g| g.id == id) {
                                    let (_, events) = group_chip(ui, c, group);
                                    out.groups.extend(events.into_iter().map(|e| (id, e)));
                                }
                            }
                            StripItem::Tab(i) => {
                                let Some(tab) = tabs.get(i) else { continue };
                                let (rect, row) = tab_chip(ui, c, tab, width, containers, groups, windows);
                                if tab.active && !ui.is_rect_visible(rect) {
                                    ui.scroll_to_rect(rect, None);
                                }
                                if tab.split.is_some() {
                                    split_rects.push(rect);
                                }
                                out.chips.push((i, rect));
                                out.tabs.push((i, row));
                            }
                        }
                    }
                    // Where a dragged tab would land.
                    if let Some(at) = drop_marker {
                        let x = match out.chips.iter().find(|(i, _)| *i >= at) {
                            Some((_, r)) => r.min.x - 1.0,
                            None => out.chips.last().map_or(ui.min_rect().min.x, |(_, r)| r.max.x + 1.0),
                        };
                        let y = out.chips.first().map_or(ui.min_rect().y_range(), |(_, r)| r.y_range());
                        ui.painter().vline(x, y, egui::Stroke::new(2.0, c.accent));
                    }
                    // The split pair: one outline round both chips when
                    // they sit side by side, one each otherwise.
                    let stroke = egui::Stroke::new(1.5, c.accent);
                    let outline = |r: egui::Rect| r.shrink2(egui::vec2(0.5, 1.5));
                    match split_rects.as_slice() {
                        [a, b] if (b.min.x - a.max.x).abs() < 6.0 || (a.min.x - b.max.x).abs() < 6.0 => {
                            ui.painter().rect_stroke(outline(a.union(*b)), egui::CornerRadius::same(8), stroke, egui::StrokeKind::Inside);
                        }
                        rects => {
                            for r in rects {
                                ui.painter().rect_stroke(outline(*r), egui::CornerRadius::same(8), stroke, egui::StrokeKind::Inside);
                            }
                        }
                    }
                });
            });
        let plus = egui::Button::new(icon(ICON_PLUS, 14.0).color(c.text))
            .frame(false)
            .min_size(egui::vec2(28.0, 28.0))
            .corner_radius(egui::CornerRadius::same(8));
        if ui.add(plus).on_hover_text("New tab (Ctrl+T)").clicked() {
            out.new_tab = Some(ContainerChoice::None);
        }
        let picker = egui::Button::new(icon(egui_phosphor::regular::CARET_DOWN, 12.0).color(c.muted))
            .frame(false)
            .min_size(egui::vec2(22.0, 28.0))
            .corner_radius(egui::CornerRadius::same(8));
        let picker = ui.add(picker).on_hover_text("New tab in a container");
        egui::Popup::menu(&picker).width(220.0).show(|ui| {
            if let Some(pick) = container_menu(ui, c, containers, "New tab") {
                out.new_tab = Some(pick);
            }
            ui.separator();
            if menu_item(ui, c, egui_phosphor::regular::STACK, "Manage containers").clicked() {
                out.manage_containers = true;
                ui.close();
            }
            if let Some(id) = presets_submenu(ui, c, "Open preset in new window") {
                out.open_preset = Some(id);
            }
        });
        let all = egui::Button::new(icon(egui_phosphor::regular::SQUARES_FOUR, 14.0).color(c.muted))
            .frame(false)
            .min_size(egui::vec2(28.0, 28.0))
            .corner_radius(egui::CornerRadius::same(8));
        if ui.add(all).on_hover_text("Show all tabs (Ctrl+Shift+\\)").clicked() {
            out.overview = true;
        }
    });
    out
}

/// One tab in the strip. Returns its rectangle and what happened.
fn tab_chip(ui: &mut egui::Ui, c: &Colors, tab: &TabInfo, width: f32, containers: &[Container], groups: &[GroupInfo], windows: WindowList) -> (egui::Rect, TabRow) {
    // Dragged to reorder it, or out of the strip to move it to a window.
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click_and_drag());
    let hovered = resp.hovered();
    let painter = ui.painter();
    let corners = egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
    if tab.active {
        painter.rect_filled(rect, corners, c.tab_active);
    } else if hovered {
        painter.rect_filled(rect.shrink2(egui::vec2(0.0, 2.0)), egui::CornerRadius::same(7), c.hover);
    } else if tab.split.is_none() {
        // A thin divider between inactive tabs, as in Chrome.
        let x = rect.max.x + 1.0;
        painter.vline(x, (rect.min.y + 8.0)..=(rect.max.y - 8.0), egui::Stroke::new(1.0, c.line));
    }
    if let Some(ctr) = tab.container {
        let line = egui::Rect::from_min_size(egui::pos2(rect.min.x + 8.0, rect.min.y), egui::vec2(rect.width() - 16.0, 3.0));
        painter.rect_filled(line, egui::CornerRadius::same(2), ctr.color32());
    }
    if let Some((_, color)) = tab.group {
        // The group's underline runs along all its tabs.
        let line = egui::Rect::from_min_max(egui::pos2(rect.min.x - 2.0, rect.max.y - 2.0), egui::pos2(rect.max.x + 2.0, rect.max.y));
        painter.rect_filled(line, 0.0, color);
    }
    let narrow = width < 80.0;
    let show_close = tab.active || (hovered && !narrow);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(if narrow { 6.0 } else { 10.0 }, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 6.0;
    tab_icon(&mut child, c, tab);
    let mut out = TabRow::default();
    if !narrow || tab.active {
        let close_w = if show_close { 22.0 } else { 0.0 };
        let label_w = (child.available_width() - close_w).max(0.0);
        if label_w > 8.0 {
            let color = if tab.unloaded { c.muted } else { c.text };
            let label = egui::Label::new(egui::RichText::new(tab.title).size(12.5).color(color))
                .truncate()
                .selectable(false);
            child.allocate_ui_with_layout(egui::vec2(label_w, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(label_w);
                ui.add(label);
            });
        }
        if show_close {
            let close = egui::Button::new(icon(ICON_XMARK, 10.0).color(c.muted))
                .frame(false)
                .min_size(egui::vec2(18.0, 18.0))
                .corner_radius(egui::CornerRadius::same(9));
            if child.add(close).on_hover_text("Close tab (Ctrl+W)").clicked() {
                out.close = true;
            }
        }
    }
    let mut tip = tab.title.to_string();
    if let Some(ctr) = tab.container {
        tip.push('\n');
        tip.push_str(&ctr.name);
    }
    if let Some(g) = tab.group.and_then(|(id, _)| groups.iter().find(|g| g.id == id)).filter(|g| !g.name.is_empty()) {
        tip.push_str("\nGroup: ");
        tip.push_str(g.name);
    }
    if tab.split.is_some() {
        tip.push_str("\nIn split view");
    }
    let resp = if tab.tooltip { resp.on_hover_text(tip) } else { resp };
    resp.context_menu(|ui| tab_menu(ui, c, tab, containers, groups, windows, &mut out));
    out.clicked = resp.clicked() && !out.close;
    out.close |= resp.middle_clicked();
    out.hovered = resp.hovered() || resp.contains_pointer();
    out.rect = Some(rect);
    out.drag_started = resp.drag_started_by(egui::PointerButton::Primary);
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    (rect, out)
}

/// The container badge at the start of the toolbar: the active tab's
/// container, in its colour. Returns whether it was clicked.
pub fn container_badge(ui: &mut egui::Ui, ctr: &Container) -> bool {
    let color = ctr.color32();
    let text = egui::RichText::new(format!("{}  {}", ctr.glyph(), ctr.name)).size(12.5).color(color);
    let button = egui::Button::new(text)
        .fill(color.gamma_multiply(0.16))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(egui::CornerRadius::same(12))
        .min_size(egui::vec2(0.0, 26.0));
    let tip = if ctr.temporary {
        "Temporary container: its cookies and site data are deleted when its last tab closes"
    } else {
        "This tab's container: its own cookies, logins and site data"
    };
    ui.add(button).on_hover_text(tip).clicked()
}

/// Stable id of the find bar's text field.
pub fn find_id() -> egui::Id {
    egui::Id::new("bc-find")
}

/// What the user did in the find bar.
pub enum FindEvent {
    Changed,
    Next,
    Previous,
    Close,
}

/// The find-in-page bar: the text, the match count, previous / next and
/// close.
pub fn find_bar(ui: &mut egui::Ui, c: &Colors, query: &mut String, matches: Option<u32>, focus: bool) -> Vec<FindEvent> {
    let mut events = Vec::new();
    ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        ui.label(icon(egui_phosphor::regular::MAGNIFYING_GLASS, 14.0).color(c.muted));
        let response = ui.add(
            egui::TextEdit::singleline(query)
                .id(find_id())
                .desired_width(260.0)
                .hint_text("Find in page"),
        );
        if focus {
            response.request_focus();
        }
        if response.changed() {
            events.push(FindEvent::Changed);
        }
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if enter {
            events.push(if ui.input(|i| i.modifiers.shift) { FindEvent::Previous } else { FindEvent::Next });
            response.request_focus();
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            events.push(FindEvent::Close);
        }
        let status = match matches {
            _ if query.is_empty() => String::new(),
            None => String::new(),
            Some(0) => "No matches".into(),
            Some(1) => "1 match".into(),
            Some(n) if n >= 1000 => "1000+ matches".into(),
            Some(n) => format!("{n} matches"),
        };
        ui.label(egui::RichText::new(status).size(12.5).color(c.muted));
        if icon_button(ui, true, egui_phosphor::regular::CARET_UP, "Previous match (Shift+Enter)", 14.0).clicked() {
            events.push(FindEvent::Previous);
        }
        if icon_button(ui, true, egui_phosphor::regular::CARET_DOWN, "Next match (Enter)", 14.0).clicked() {
            events.push(FindEvent::Next);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if icon_button(ui, true, ICON_XMARK, "Close (Esc)", 13.0).clicked() {
                events.push(FindEvent::Close);
            }
        });
    });
    events
}

/// The header of the drawer: icon, title and a close button. Returns
/// whether close was clicked.
pub fn drawer_header(ui: &mut egui::Ui, c: &Colors, glyph: &str, title: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(icon(glyph, 16.0).color(c.accent));
        ui.label(egui::RichText::new(title).size(15.0).strong().color(c.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = icon_button(ui, true, ICON_XMARK, "Close panel", 13.0).clicked();
        });
    });
    close
}

/// Registers the embedded Phosphor face with egui. The font ships inside the
/// binary, so no asset lookup can fail.
fn install_icon_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    crate::icons::add_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

/// The chrome's colours, from the current [`Theme`](crate::theme::Theme).
/// The historical names map to theme tokens: `bar_bg` is the surface,
/// `sidebar_bg` the window background, `line` the border, `field_bg` the
/// address bar, `hover` the second surface.
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub bar_bg: egui::Color32,
    pub sidebar_bg: egui::Color32,
    pub line: egui::Color32,
    pub field_bg: egui::Color32,
    pub text: egui::Color32,
    pub muted: egui::Color32,
    pub hover: egui::Color32,
    pub accent: egui::Color32,
    pub accent_text: egui::Color32,
    pub danger: egui::Color32,
    pub danger_text: egui::Color32,
    pub warning: egui::Color32,
    pub success: egui::Color32,
    pub tab_active: egui::Color32,
    pub tab_inactive: egui::Color32,
    pub drawer: egui::Color32,
    /// The dimmed backdrop behind the palette.
    pub scrim: egui::Color32,
    /// Drop shadows of floating panels.
    pub shadow: egui::Color32,
    pub dark: bool,
}

impl Colors {
    pub fn from_theme(theme: &crate::theme::Theme) -> Self {
        let t = &theme.t;
        // Shadows and the scrim darken: black at an opacity that reads on
        // either kind of theme.
        let (scrim, shadow) = if theme.dark { (110, 160) } else { (70, 60) };
        Self {
            bar_bg: rgb(t.surface),
            sidebar_bg: rgb(t.background),
            line: rgb(t.border),
            field_bg: rgb(t.address_bar),
            text: rgb(t.text),
            muted: rgb(t.text_muted),
            hover: rgb(t.surface2),
            accent: rgb(t.accent),
            accent_text: rgb(t.accent_text),
            danger: rgb(t.danger),
            danger_text: rgb(t.danger_text),
            warning: rgb(t.warning),
            success: rgb(t.success),
            tab_active: rgb(t.tab_active),
            tab_inactive: rgb(t.tab_inactive),
            drawer: rgb(t.drawer),
            scrim: egui::Color32::from_black_alpha(scrim),
            shadow: egui::Color32::from_black_alpha(shadow),
            dark: theme.dark,
        }
    }

    /// The default dark (Late Night) or light theme.
    #[cfg(test)]
    pub fn for_dark(dark: bool) -> Self {
        Self::from_theme(if dark { &crate::theme::DARK } else { &crate::theme::LIGHT })
    }
}

// Tab groups take their colours from the theme, like the rest of the chrome.
impl Colors {
    /// A dark theme: light text on dark bars.
    pub fn is_dark(&self) -> bool {
        let lum = |c: egui::Color32| u32::from(c.r()) * 3 + u32::from(c.g()) * 6 + u32::from(c.b());
        lum(self.text) > lum(self.bar_bg)
    }

    /// The shade of a tab group colour that reads on this theme.
    pub fn group_color(&self, color: GroupColor) -> egui::Color32 {
        let (light, dark) = color.shades();
        rgb(if self.is_dark() { dark } else { light })
    }
}

pub fn rgb(hex: u32) -> egui::Color32 {
    egui::Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Installs the chrome visuals and spacing for `c`. Called once per frame
/// before the panels are built.
pub fn apply_style(ctx: &egui::Context, c: &Colors) {
    let mut v = if c.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = c.bar_bg;
    v.window_fill = c.bar_bg;
    v.window_stroke = egui::Stroke::new(1.0, c.line);
    v.window_shadow.color = c.shadow;
    v.popup_shadow.color = c.shadow;
    v.extreme_bg_color = c.field_bg;
    v.faint_bg_color = c.sidebar_bg;
    v.code_bg_color = c.hover;
    v.hyperlink_color = c.accent;
    v.warn_fg_color = c.warning;
    v.error_fg_color = c.danger;
    v.selection.bg_fill = c.accent.gamma_multiply(0.40);
    v.selection.stroke = egui::Stroke::new(1.0, c.text);
    v.override_text_color = None;

    let radius = egui::CornerRadius::same(9);
    v.widgets.noninteractive.corner_radius = radius;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, c.text);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, c.line);
    v.widgets.noninteractive.bg_fill = c.bar_bg;
    v.widgets.noninteractive.weak_bg_fill = c.bar_bg;

    v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    v.widgets.inactive.bg_fill = c.hover;
    v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    v.widgets.inactive.corner_radius = radius;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, c.text);

    v.widgets.hovered.weak_bg_fill = c.hover;
    v.widgets.hovered.bg_fill = c.hover;
    v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
    v.widgets.hovered.corner_radius = radius;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, c.text);

    v.widgets.active.weak_bg_fill = c.accent;
    v.widgets.active.bg_fill = c.accent;
    v.widgets.active.bg_stroke = egui::Stroke::NONE;
    v.widgets.active.corner_radius = radius;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, c.accent_text);

    v.widgets.open.weak_bg_fill = c.hover;
    v.widgets.open.bg_fill = c.hover;
    v.widgets.open.fg_stroke = egui::Stroke::new(1.0, c.text);

    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(4.0, 4.0);
        s.spacing.button_padding = egui::vec2(7.0, 3.0);
        s.spacing.interact_size = egui::vec2(28.0, 28.0);
    });
}

// ---- password manager ----

/// What the key menu shows.
pub struct PasswordMenu<'a> {
    pub status: &'a crate::passwords::Status,
    /// Logins saved for the current site (empty while locked).
    pub logins: &'a [crate::vault::Credential],
    /// The kind of password form the page has, if any.
    pub forms: Option<crate::passwords::FormKind>,
    /// A login waiting to be saved.
    pub offer: Option<&'a crate::passwords::Offer>,
    pub on_web_page: bool,
    pub host: &'a str,
}

pub enum PasswordEvent {
    Fill(String),
    Suggest,
    Save,
    Manage,
    Lock,
}

/// A full-width flat button for the key menu.
fn menu_button(ui: &mut egui::Ui, c: &Colors, text: &str) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 30.0],
        egui::Button::new(egui::RichText::new(text).size(13.0).color(c.text))
            .fill(c.hover)
            .corner_radius(egui::CornerRadius::same(9)),
    )
}

/// The contents of the password menu under the key icon.
pub fn password_menu(
    ui: &mut egui::Ui,
    c: &Colors,
    menu: &PasswordMenu,
    emit: &mut dyn FnMut(PasswordEvent),
) {
    use crate::passwords::{FormKind, Status};
    ui.set_min_width(280.0);
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.horizontal(|ui| {
        ui.label(icon(ICON_KEY, 16.0).color(c.accent));
        ui.label(egui::RichText::new("Passwords").size(15.0).strong().color(c.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (label, color) = match menu.status {
                Status::Unlocked { .. } => ("Unlocked", c.accent),
                Status::Locked => ("Locked", c.muted),
                Status::NoVault => ("Not set up", c.muted),
            };
            ui.label(egui::RichText::new(label).size(12.0).color(color));
        });
    });
    ui.separator();
    match menu.status {
        Status::NoVault => {
            ui.label(
                egui::RichText::new("Keep your logins in a KeePass (.kdbx) vault, protected by a password or a key file.")
                    .size(12.5)
                    .color(c.muted),
            );
            if menu_button(ui, c, "Set up the password manager").clicked() {
                emit(PasswordEvent::Manage);
                ui.close();
            }
        }
        Status::Locked => {
            ui.label(egui::RichText::new("Your vault is locked.").size(12.5).color(c.muted));
            if menu_button(ui, c, &format!("{}  Unlock vault", ICON_KEY)).clicked() {
                emit(PasswordEvent::Manage);
                ui.close();
            }
        }
        Status::Unlocked { logins } => {
            if menu.on_web_page {
                if menu.logins.is_empty() {
                    let text = if menu.host.is_empty() {
                        "Nothing to fill on this page.".to_string()
                    } else {
                        format!("No saved logins for {}.", menu.host)
                    };
                    ui.label(egui::RichText::new(text).size(12.5).color(c.muted));
                }
                for login in menu.logins {
                    let name = if login.username.is_empty() { &login.title } else { &login.username };
                    let label = format!("{}  {}", crate::icons::USER, name);
                    if menu_button(ui, c, &label)
                        .on_hover_text(format!("Fill {} on this page", login.title))
                        .clicked()
                    {
                        emit(PasswordEvent::Fill(login.id.clone()));
                        ui.close();
                    }
                }
                if matches!(menu.forms, Some(FormKind::SignUp | FormKind::ChangePassword))
                    && menu_button(ui, c, &format!("{}  Suggest a strong password", crate::icons::LOCK_KEY)).clicked()
                {
                    emit(PasswordEvent::Suggest);
                    ui.close();
                }
                if menu.offer.is_some() && menu_button(ui, c, &format!("{}  Save this login", crate::icons::CHECK)).clicked() {
                    emit(PasswordEvent::Save);
                    ui.close();
                }
            } else {
                ui.label(egui::RichText::new("Open a web page to fill a login.").size(12.5).color(c.muted));
            }
            ui.separator();
            if menu_button(ui, c, &format!("{}  Manage passwords ({logins})", crate::icons::FOLDER)).clicked() {
                emit(PasswordEvent::Manage);
                ui.close();
            }
            if menu_button(ui, c, &format!("{}  Lock vault", crate::icons::LOCK_KEY)).clicked() {
                emit(PasswordEvent::Lock);
                ui.close();
            }
        }
    }
}

pub enum SaveBarEvent {
    Save,
    OpenPasswords,
    NotNow,
    Never,
}

/// The bar offering to save (or update) a login the page just submitted.
pub fn save_login_bar(
    ui: &mut egui::Ui,
    c: &Colors,
    offer: &crate::passwords::Offer,
    status: &crate::passwords::Status,
    emit: &mut dyn FnMut(SaveBarEvent),
) {
    use crate::passwords::Status;
    // The page chose the user name: show it cleaned and short.
    let name = crate::browser::display_text(&offer.username, 64);
    let who = if name.is_empty() {
        String::new()
    } else if name.chars().count() < offer.username.chars().count() && name.chars().count() == 64 {
        format!(" ({name}…)")
    } else {
        format!(" ({name})")
    };
    let (text, action_label, action) = match status {
        Status::Unlocked { .. } if offer.update_id.is_some() => (
            format!("Update the saved password for {}{who}?", offer.host),
            "Update",
            SaveBarEvent::Save,
        ),
        Status::Unlocked { .. } => (
            if offer.kind == crate::passwords::FormKind::SignUp {
                format!("Save the new account for {}{who}?", offer.host)
            } else {
                format!("Save the login for {}{who}?", offer.host)
            },
            "Save",
            SaveBarEvent::Save,
        ),
        Status::Locked => (
            format!("Unlock your vault to save the login for {}{who}.", offer.host),
            "Unlock",
            SaveBarEvent::OpenPasswords,
        ),
        Status::NoVault => (
            format!("Set up a password vault to save the login for {}{who}.", offer.host),
            "Set up",
            SaveBarEvent::OpenPasswords,
        ),
    };
    ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        ui.label(icon(ICON_KEY, 16.0).color(c.accent));
        ui.label(egui::RichText::new(text).size(13.0).color(c.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Never for this site").size(12.5).color(c.muted))
                        .frame(false),
                )
                .clicked()
            {
                emit(SaveBarEvent::Never);
            }
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Not now").size(12.5).color(c.text))
                        .fill(c.bar_bg)
                        .corner_radius(egui::CornerRadius::same(8)),
                )
                .clicked()
            {
                emit(SaveBarEvent::NotNow);
            }
            let primary = egui::Button::new(egui::RichText::new(action_label).size(12.5).color(c.accent_text))
                .fill(c.accent)
                .corner_radius(egui::CornerRadius::same(8));
            if ui.add(primary).clicked() {
                match action {
                    SaveBarEvent::Save => emit(SaveBarEvent::Save),
                    _ => emit(SaveBarEvent::OpenPasswords),
                }
            }
        });
    });
}

/// A site's permission prompt: "<host> wants to use your microphone and
/// camera" with Allow / Block and "Remember for this site". Returns the
/// answer (`true`: allow). Until `armed`, the buttons are shown disabled.
pub fn permission_bar(ui: &mut egui::Ui, c: &Colors, prompt: &crate::permissions::Prompt, remember: &mut bool, armed: bool) -> Option<bool> {
    use crate::permissions::Kind;
    use egui_phosphor::regular as ph;
    let glyph = match prompt.kinds.first() {
        Some(Kind::Camera) => ph::VIDEO_CAMERA,
        Some(Kind::Microphone) => ph::MICROPHONE,
        Some(Kind::Screen) => ph::MONITOR,
        Some(Kind::Location) => ph::MAP_PIN,
        Some(Kind::Notifications) => ph::BELL,
        Some(Kind::Clipboard) => ph::CLIPBOARD,
        _ => ph::SHIELD_CHECK,
    };
    let mut answer = None;
    ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        ui.label(icon(glyph, 16.0).color(c.accent));
        ui.label(egui::RichText::new(prompt.text()).size(13.0).color(c.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            let block = egui::Button::new(egui::RichText::new("Block").size(12.5).color(c.text))
                .fill(c.bar_bg)
                .corner_radius(egui::CornerRadius::same(8));
            if ui.add_enabled(armed, block).clicked() {
                answer = Some(false);
            }
            let allow = egui::Button::new(egui::RichText::new("Allow").size(12.5).color(c.accent_text))
                .fill(c.accent)
                .corner_radius(egui::CornerRadius::same(8));
            if ui.add_enabled(armed, allow).clicked() {
                answer = Some(true);
            }
            if prompt.rememberable() {
                ui.checkbox(remember, egui::RichText::new("Remember for this site").size(12.5).color(c.text));
            } else {
                ui.label(egui::RichText::new("Asked every time").size(12.0).color(c.muted))
                    .on_hover_text("A block is remembered; screen sharing is never allowed without asking");
                ui.checkbox(remember, egui::RichText::new("Remember a block").size(12.5).color(c.text));
            }
        });
    });
    answer
}

/// The one-time notice on call sites when WebKitGTK has no WebRTC.
/// Returns `Some(true)` to open the page in the default browser,
/// `Some(false)` to close the notice.
pub fn webrtc_bar(ui: &mut egui::Ui, c: &Colors, browser: &str) -> Option<bool> {
    let mut answer = None;
    ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        ui.label(icon(egui_phosphor::regular::PHONE_X, 16.0).color(c.accent));
        ui.label(
            egui::RichText::new("Calls need WebRTC, which this system's WebKitGTK doesn't include")
                .size(13.0)
                .color(c.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            if ui.add(egui::Button::new(icon(crate::icons::CLOSE, 13.0).color(c.muted)).frame(false)).clicked() {
                answer = Some(false);
            }
            let open = egui::Button::new(egui::RichText::new(format!("Open in {browser}")).size(12.5).color(c.accent_text))
                .fill(c.accent)
                .corner_radius(egui::CornerRadius::same(8));
            if ui.add(open).clicked() {
                answer = Some(true);
            }
        });
    });
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_page_accepts_pointer_but_chrome_drags_and_popups_do_not() {
        use egui::{Event, PointerButton, Pos2, Rect, Vec2};
        let ctx = egui::Context::default();
        let page_pos = egui::pos2(400.0, 300.0);
        let mut button = Rect::NOTHING;
        let mut frame = |events: Vec<Event>| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::Panel::top("toolbar").exact_size(44.0).show(ui, |ui| {
                    let response = ui.add(egui::Button::new("Menu").sense(egui::Sense::click_and_drag()));
                    button = response.rect;
                    egui::Popup::menu(&response).show(|ui| {
                        ui.label("Menu above the page");
                    });
                });
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.painter().rect_filled(ui.max_rect(), 0.0, egui::Color32::WHITE);
                });
            });
            output.textures_delta.clear();
            button.center()
        };
        let button_pos = frame(vec![]);
        frame(vec![Event::PointerMoved(page_pos)]);
        // This is why forwarding only !consumed drops legitimate page clicks.
        assert!(ctx.egui_wants_pointer_input());
        assert!(!blocks_page_pointer(&ctx, page_pos));

        let press = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![Event::PointerMoved(button_pos)]);
        frame(vec![press(button_pos, true)]);
        frame(vec![Event::PointerMoved(page_pos)]);
        assert!(blocks_page_pointer(&ctx, page_pos), "chrome owns this drag");
        frame(vec![press(page_pos, false)]);
        frame(vec![]);
        assert!(!blocks_page_pointer(&ctx, page_pos));

        frame(vec![Event::PointerMoved(button_pos)]);
        frame(vec![press(button_pos, true)]);
        frame(vec![press(button_pos, false)]);
        frame(vec![Event::PointerMoved(page_pos)]);
        assert!(egui::Popup::is_any_open(&ctx));
        assert!(blocks_page_pointer(&ctx, page_pos), "dismissal must not click through");
    }

    /// Builds the toolbar the way `browser.rs` does (buttons on the left,
    /// then the right-hand buttons anchored to the right edge with the
    /// address bar taking the rest) and returns the rect of every widget.
    fn toolbar_rects(screen_w: f32) -> (Vec<egui::Rect>, egui::Rect) {
        let ctx = egui::Context::default();
        install_icon_font(&ctx);
        let c = Colors::for_dark(true);
        let mut text = String::from("https://example.com/");
        let mut buttons = Vec::new();
        let mut address = egui::Rect::NOTHING;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen_w, 400.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::Panel::top("toolbar").exact_size(44.0).show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(8.0);
                    for code in [ICON_BACK, ICON_FORWARD, ICON_RELOAD, ICON_HOME] {
                        buttons.push(icon_button(ui, true, code, "x", 16.0).rect);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(8.0);
                        for code in [ICON_KEY, ICON_GEAR, crate::icons::THEME] {
                            buttons.push(icon_button(ui, true, code, "x", 16.0).rect);
                        }
                        let width = (ui.available_width() - 4.0).max(60.0);
                        let (resp, _) = address_bar(ui, &mut text, width, &c, ICON_LOCK, Some(false), Some(110));
                        address = resp.rect;
                    });
                });
            });
        });
        output.textures_delta.clear();
        (buttons, address)
    }

    #[test]
    fn toolbar_buttons_stay_inside_the_window_at_every_width() {
        for width in [1600.0, 1280.0, 900.0, 640.0, 520.0] {
            let (buttons, address) = toolbar_rects(width);
            assert_eq!(buttons.len(), 7);
            for (i, r) in buttons.iter().enumerate() {
                assert!(r.max.x <= width + 0.5, "button {i} ends at {} in a {width}px window", r.max.x);
                assert!(r.min.x >= 0.0, "button {i} starts left of the window at {width}px");
            }
            // The right-hand buttons are ordered theme | settings | key, key last.
            let (key, gear, theme) = (buttons[4], buttons[5], buttons[6]);
            assert!(key.min.x > gear.min.x && gear.min.x > theme.min.x || key.max.x >= width - 20.0,
                "key is the rightmost button at {width}px");
            assert!(key.max.x >= width - 20.0, "key hugs the right edge at {width}px: {}", key.max.x);
            // The address bar never overlaps the buttons beside it.
            if width >= 640.0 {
                assert!(address.max.x <= theme.min.x.min(gear.min.x).min(key.min.x) + 0.5, "address overlaps buttons at {width}px");
                assert!(address.width() > 150.0, "address bar too small at {width}px");
            }
        }
    }

    #[test]
    fn tabs_shrink_like_chrome_and_never_vanish() {
        assert_eq!(tab_width(1000.0, 2), 220.0);
        assert_eq!(tab_width(1000.0, 10), 98.0);
        assert_eq!(tab_width(1000.0, 100), 48.0, "the strip scrolls instead");
        assert_eq!(tab_width(1000.0, 0), 220.0);
    }

    /// Draws the tab strip with `count` tabs in a `screen_w` window and
    /// returns the right edge of the last widget.
    fn strip_right_edge(screen_w: f32, count: usize) -> f32 {
        let ctx = egui::Context::default();
        install_icon_font(&ctx);
        let c = Colors::for_dark(true);
        let ctr = Container { id: "a".into(), name: "Work".into(), color: "red".into(), icon: "briefcase".into(), temporary: false };
        let titles: Vec<String> = (0..count).map(|i| format!("A rather long page title {i}")).collect();
        let tabs: Vec<TabInfo> = titles
            .iter()
            .enumerate()
            .map(|(i, t)| TabInfo {
                title: t,
                active: i == 0,
                container: (i % 2 == 1).then_some(&ctr),
                favicon: None,
                unloaded: i == 2,
                // Tabs 1 and 2 in a group; 0 and 1 in a split view.
                group: (1..=2).contains(&i).then_some((5, c.group_color(GroupColor::Blue))),
                split: match i {
                    0 => Some(Side::Left),
                    1 => Some(Side::Right),
                    _ => None,
                },
                can_split: i != 0,
                tooltip: true,
            })
            .collect();
        let groups = [GroupInfo { id: 5, name: "Research", color: GroupColor::Blue, collapsed: false, tabs: 2, edit: false }];
        let items: Vec<StripItem> = (0..count)
            .flat_map(|i| if i == 1 { vec![StripItem::Group(5), StripItem::Tab(i)] } else { vec![StripItem::Tab(i)] })
            .collect();
        let mut right = 0.0f32;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen_w, 400.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::Panel::top("tabstrip").exact_size(38.0).show(ui, |ui| {
                let strip = tab_strip(ui, &c, &tabs, &items, &groups, std::slice::from_ref(&ctr), &[], None);
                assert_eq!(strip.tabs.len(), count);
                right = ui.min_rect().max.x;
            });
        });
        output.textures_delta.clear();
        right
    }

    #[test]
    fn tab_strip_fits_the_window() {
        for (width, count) in [(1280.0, 3), (1280.0, 40), (640.0, 12), (480.0, 1)] {
            let right = strip_right_edge(width, count);
            assert!(right <= width + 0.5, "{count} tabs end at {right} in a {width}px window");
        }
    }

    /// The width the bookmarks bar's content really takes in a window of
    /// `screen_w` with `count` bookmarks.
    fn bookmarks_bar_used(screen_w: f32, count: usize) -> f32 {
        let ctx = egui::Context::default();
        install_icon_font(&ctx);
        let c = Colors::for_dark(true);
        // Every third item is a folder.
        let items: Vec<crate::bookmarks::Node> = (0..count)
            .map(|i| crate::bookmarks::Node {
                id: i as u64 + 3,
                title: format!("A fairly long bookmark title number {i}"),
                url: (i % 3 != 0).then(|| format!("https://site{i}.test/")),
                children: Vec::new(),
                added: 0,
                import: 0,
            })
            .collect();
        let mut used = 0.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(screen_w, 400.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::Panel::top("bookmarks").exact_size(34.0).show(ui, |ui| {
                let _ = bookmarks_bar(ui, &c, &items);
                used = ui.min_rect().max.x;
            });
        });
        output.textures_delta.clear();
        used
    }

    #[test]
    fn bookmarks_bar_fits_the_window_with_few_or_many_bookmarks() {
        for width in [1600.0, 1280.0, 800.0, 500.0] {
            for count in [0, 3, 40] {
                let used = bookmarks_bar_used(width, count);
                assert!(used <= width + 0.5, "{count} bookmarks overflow a {width}px window: ends at {used}");
            }
        }
    }
}
