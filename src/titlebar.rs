//! The browser's own window frame (no system title bar): a draggable title
//! bar row with the window buttons on the right, and resizing from every
//! edge and corner.
//!
//! The widgets only report [`WindowOp`]s; the browser applies them to the
//! winit window ([`apply`]). The approach (a drag band drawn under the
//! controls, caption buttons drawn with strokes, resize bands above every
//! layer) is adapted from ArtCraft Launcher's `crates/ui-egui/src/widgets.rs`
//! (MIT OR Apache-2.0, see THIRD_PARTY_NOTICES.md).

use egui::{CornerRadius, Rect, Sense, Stroke, vec2};
use winit::window::{ResizeDirection, Window};

use crate::egui_chrome::Colors;

/// How wide (in points) the invisible resize band along the window's edges
/// is. The page and the web view are inset by it, so the band is never
/// under the native web view.
pub const RESIZE_EDGE: f32 = 5.0;

/// The width of one window button.
pub const BUTTON_W: f32 = 46.0;

/// The three window buttons.
pub const BUTTONS_W: f32 = 3.0 * BUTTON_W;

/// Empty title bar kept free next to the tabs, so there is always something
/// to drag the window by.
pub const DRAG_GAP: f32 = 36.0;

/// What the user did to the window through the browser's own frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowOp {
    /// Move the window (the pointer is down on the title bar).
    Drag,
    /// Resize it from an edge or a corner (the pointer is down there).
    Resize(ResizeDirection),
    ToggleMaximize,
    Minimize,
    Close,
    /// The window menu's "New window" and "Name window…" (the browser's).
    NewWindow,
    NameWindow,
}

/// Applies `op` to `window`. `Close` is the caller's (it ends the event
/// loop). Returns whether the window manager took the pointer (a move or a
/// resize started), after which no button release reaches the window.
pub fn apply(window: &Window, op: WindowOp) -> bool {
    match op {
        WindowOp::Drag => window.drag_window().is_ok(),
        WindowOp::Resize(direction) => window.drag_resize_window(direction).is_ok(),
        WindowOp::ToggleMaximize => {
            window.set_maximized(!window.is_maximized());
            false
        }
        WindowOp::Minimize => {
            window.set_minimized(true);
            false
        }
        WindowOp::Close | WindowOp::NewWindow | WindowOp::NameWindow => false,
    }
}

/// Which way a press at `pos` resizes a window whose content is `rect`, with
/// an `edge`-wide band. A corner takes a little of each edge next to it.
pub fn resize_direction(rect: Rect, pos: egui::Pos2, edge: f32) -> Option<ResizeDirection> {
    use ResizeDirection as D;
    if edge <= 0.0 || !rect.contains(pos) {
        return None;
    }
    let near = |d: f32| d < edge;
    let corner = |d: f32| d < 3.0 * edge;
    let (left, right, top, bottom) = (pos.x - rect.left(), rect.right() - pos.x, pos.y - rect.top(), rect.bottom() - pos.y);
    if (near(top) && corner(left)) || (near(left) && corner(top)) {
        Some(D::NorthWest)
    } else if (near(top) && corner(right)) || (near(right) && corner(top)) {
        Some(D::NorthEast)
    } else if (near(bottom) && corner(left)) || (near(left) && corner(bottom)) {
        Some(D::SouthWest)
    } else if (near(bottom) && corner(right)) || (near(right) && corner(bottom)) {
        Some(D::SouthEast)
    } else if near(top) {
        Some(D::North)
    } else if near(bottom) {
        Some(D::South)
    } else if near(left) {
        Some(D::West)
    } else if near(right) {
        Some(D::East)
    } else {
        None
    }
}

/// The pointer shape over a resize band.
pub fn cursor_for(direction: ResizeDirection) -> egui::CursorIcon {
    use ResizeDirection as D;
    use egui::CursorIcon as C;
    match direction {
        D::North => C::ResizeNorth,
        D::South => C::ResizeSouth,
        D::East => C::ResizeEast,
        D::West => C::ResizeWest,
        D::NorthEast => C::ResizeNorthEast,
        D::NorthWest => C::ResizeNorthWest,
        D::SouthEast => C::ResizeSouthEast,
        D::SouthWest => C::ResizeSouthWest,
    }
}

/// The empty part of a title bar row: dragging it moves the window,
/// double-clicking maximizes or restores it, right-clicking opens the window
/// menu. Call it before drawing the row's own widgets, which stay on top.
pub fn drag_area(ui: &mut egui::Ui, rect: Rect, c: &Colors, maximized: bool, ops: &mut Vec<WindowOp>) {
    let resp = ui.interact(rect, egui::Id::new("bc-title-bar"), Sense::click_and_drag());
    if resp.drag_started_by(egui::PointerButton::Primary) {
        ops.push(WindowOp::Drag);
    }
    if resp.double_clicked() {
        ops.push(WindowOp::ToggleMaximize);
    }
    resp.context_menu(|ui| window_menu(ui, c, maximized, ops));
}

/// The window menu: new window, name the window, minimize, maximize or
/// restore, close.
pub fn window_menu(ui: &mut egui::Ui, c: &Colors, maximized: bool, ops: &mut Vec<WindowOp>) {
    use egui_phosphor::regular as ph;
    for (glyph, label, op) in [(ph::APP_WINDOW, "New window", WindowOp::NewWindow), (ph::TAG, "Name window\u{2026}", WindowOp::NameWindow)] {
        if crate::egui_chrome::menu_item(ui, c, glyph, label).clicked() {
            ops.push(op);
            ui.close();
        }
    }
    ui.separator();
    let size = if maximized { (ph::CORNERS_IN, "Restore") } else { (ph::CORNERS_OUT, "Maximize") };
    for (glyph, label, op) in [
        (ph::MINUS, "Minimize", WindowOp::Minimize),
        (size.0, size.1, WindowOp::ToggleMaximize),
        (ph::X, "Close", WindowOp::Close),
    ] {
        if crate::egui_chrome::menu_item(ui, c, glyph, label).clicked() {
            ops.push(op);
            ui.close();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Minimize, maximize (or restore) and close, `height` tall, for a
/// right-to-left layout: close ends up at the right. `edge` is the resize
/// band (0 when maximized), which the buttons leave to the resize.
pub fn window_buttons(ui: &mut egui::Ui, c: &Colors, height: f32, maximized: bool, edge: f32, ops: &mut Vec<WindowOp>) {
    if window_button(ui, c, Button::Close, height, edge).clicked() {
        ops.push(WindowOp::Close);
    }
    let size = if maximized { Button::Restore } else { Button::Maximize };
    if window_button(ui, c, size, height, edge).clicked() {
        ops.push(WindowOp::ToggleMaximize);
    }
    if window_button(ui, c, Button::Minimize, height, edge).clicked() {
        ops.push(WindowOp::Minimize);
    }
}

fn window_button(ui: &mut egui::Ui, c: &Colors, button: Button, height: f32, edge: f32) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(BUTTON_W, height), Sense::hover());
    // Clickable below the resize band along the top (and, for close, the
    // right edge); maximized there is no band and close takes the corner.
    let right = if button == Button::Close { edge } else { 0.0 };
    let hit = Rect::from_min_max(rect.min + vec2(0.0, edge), rect.max - vec2(right, 0.0));
    let resp = ui.interact(hit, egui::Id::new(("bc-window-button", button as u8)), Sense::click());
    let (fill, ink) = match (button, resp.hovered()) {
        (Button::Close, true) => (c.danger, c.danger_text),
        (_, true) => (c.hover, c.text),
        (_, false) => (egui::Color32::TRANSPARENT, c.muted),
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::ZERO, fill);
    let center = rect.center();
    let stroke = Stroke::new(1.0, ink);
    // Half the glyph's size: 10 px glyphs, like the desktop's own.
    let s = 5.0;
    match button {
        Button::Minimize => {
            p.hline(center.x - s..=center.x + s, center.y, stroke);
        }
        Button::Maximize => {
            p.rect_stroke(Rect::from_center_size(center, vec2(2.0 * s, 2.0 * s)), CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
        }
        Button::Restore => {
            // A window in front of another.
            let front = Rect::from_min_size(center + vec2(-s, -s + 2.0), vec2(2.0 * s - 2.0, 2.0 * s - 2.0));
            p.rect_stroke(front, CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
            p.hline(center.x - s + 2.0..=center.x + s, center.y - s, stroke);
            p.vline(center.x + s, center.y - s..=center.y + s - 2.0, stroke);
        }
        Button::Close => {
            p.line_segment([center + vec2(-s, -s), center + vec2(s, s)], stroke);
            p.line_segment([center + vec2(-s, s), center + vec2(s, -s)], stroke);
        }
    }
    let label = match button {
        Button::Minimize => "Minimize",
        Button::Maximize => "Maximize",
        Button::Restore => "Restore",
        Button::Close => "Close",
    };
    resp.on_hover_text(label)
}

/// The window's outline and its resize bands: a hairline round the window,
/// the resize cursors along its edges, and a press there starts a resize.
/// The bands sit above every layer, so a press on them never reaches the
/// widgets underneath. Nothing happens with `edge` = 0 (maximized, full
/// screen).
pub fn window_edges(ctx: &egui::Context, c: &Colors, edge: f32, ops: &mut Vec<WindowOp>) {
    if edge <= 0.0 {
        return;
    }
    let r = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("bc-window-border")))
        .rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0, c.line), egui::StrokeKind::Inside);
    let bands = [
        r.with_max_y(r.top() + edge),
        r.with_min_y(r.bottom() - edge),
        r.with_max_x(r.left() + edge),
        r.with_min_x(r.right() - edge),
    ];
    for (i, band) in bands.into_iter().enumerate() {
        egui::Area::new(egui::Id::new(("bc-window-edge", i)))
            .order(egui::Order::Tooltip)
            .movable(false)
            .constrain(false)
            .fixed_pos(band.min)
            .show(ctx, |ui| ui.allocate_response(band.size(), Sense::click()));
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    if let Some(direction) = resize_direction(r, pos, edge) {
        ctx.set_cursor_icon(cursor_for(direction));
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ops.push(WindowOp::Resize(direction));
        }
    }
}

/// A debug-build test hook: `BROWSECRAFT_WINDOW_SCRIPT="2:maximize,4:restore"`
/// runs window operations that many seconds after start-up, through the same
/// code as the title bar's buttons. Input from `xdotool` does not reach the
/// window on a GNOME session, so this is how the operations are checked.
#[cfg(debug_assertions)]
pub fn test_script() -> Vec<(std::time::Duration, String)> {
    let Ok(text) = std::env::var("BROWSECRAFT_WINDOW_SCRIPT") else {
        return Vec::new();
    };
    parse_test_script(&text)
}

/// `"2:maximize,4:move-tab:1:new"`: seconds, then the step (which may
/// contain colons; unknown steps are ignored when run). A step may end in
/// `@N` to act on window N.
#[cfg(debug_assertions)]
pub fn parse_test_script(text: &str) -> Vec<(std::time::Duration, String)> {
    text.split(',')
        .filter_map(|step| {
            let (secs, op) = step.split_once(':')?;
            let secs: f32 = secs.trim().parse().ok()?;
            let op = op.trim();
            (!op.is_empty()).then(|| (std::time::Duration::from_secs_f32(secs.max(0.0)), op.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, pos2};

    #[test]
    fn edges_and_corners_resize_the_right_way() {
        use ResizeDirection as D;
        let r = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let e = RESIZE_EDGE;
        assert_eq!(resize_direction(r, pos2(400.0, 1.0), e), Some(D::North));
        assert_eq!(resize_direction(r, pos2(400.0, 598.0), e), Some(D::South));
        assert_eq!(resize_direction(r, pos2(1.0, 300.0), e), Some(D::West));
        assert_eq!(resize_direction(r, pos2(799.0, 300.0), e), Some(D::East));
        assert_eq!(resize_direction(r, pos2(1.0, 1.0), e), Some(D::NorthWest));
        assert_eq!(resize_direction(r, pos2(10.0, 2.0), e), Some(D::NorthWest), "a corner takes some of the edge");
        assert_eq!(resize_direction(r, pos2(798.0, 12.0), e), Some(D::NorthEast));
        assert_eq!(resize_direction(r, pos2(2.0, 590.0), e), Some(D::SouthWest));
        assert_eq!(resize_direction(r, pos2(799.0, 599.0), e), Some(D::SouthEast));
        assert_eq!(resize_direction(r, pos2(400.0, 300.0), e), None);
        assert_eq!(resize_direction(r, pos2(400.0, 1.0), 0.0), None, "no band when maximized");
        assert_eq!(resize_direction(r, pos2(900.0, 1.0), e), None, "outside the window");
        for d in [D::North, D::South, D::East, D::West, D::NorthEast, D::NorthWest, D::SouthEast, D::SouthWest] {
            assert_ne!(cursor_for(d), egui::CursorIcon::Default);
        }
    }

    /// A title bar row as `browser.rs` builds it: the drag area, a "tab"
    /// button on top of it, and the window buttons on the right.
    struct Bar {
        ctx: egui::Context,
        tab: Rect,
        buttons: Vec<Rect>,
        tab_clicks: usize,
    }

    impl Bar {
        fn new() -> Self {
            let ctx = egui::Context::default();
            Bar { ctx, tab: Rect::NOTHING, buttons: Vec::new(), tab_clicks: 0 }
        }

        fn frame(&mut self, events: Vec<Event>, maximized: bool) -> Vec<WindowOp> {
            let c = Colors::for_dark(true);
            let mut ops = Vec::new();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))),
                events,
                ..Default::default()
            };
            let edge = if maximized { 0.0 } else { RESIZE_EDGE };
            let (mut tab, mut buttons, mut clicks) = (Rect::NOTHING, Vec::new(), 0);
            let mut out = self.ctx.run_ui(input, |ui| {
                egui::Panel::top("title").exact_size(38.0).frame(egui::Frame::NONE).show(ui, |ui| {
                    let full = ui.max_rect();
                    drag_area(ui, full, &c, maximized, &mut ops);
                    let resp = ui.put(Rect::from_min_size(full.min + vec2(20.0, 4.0), vec2(150.0, 30.0)), egui::Button::new("tab"));
                    tab = resp.rect;
                    clicks += usize::from(resp.clicked());
                    let controls = full.with_min_x(full.max.x - BUTTONS_W);
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::right_to_left(egui::Align::Center)));
                    child.spacing_mut().item_spacing.x = 0.0;
                    window_buttons(&mut child, &c, full.height(), maximized, edge, &mut ops);
                    buttons = vec![
                        Rect::from_min_size(pos2(full.max.x - BUTTON_W, full.min.y), vec2(BUTTON_W, full.height())),
                        Rect::from_min_size(pos2(full.max.x - 2.0 * BUTTON_W, full.min.y), vec2(BUTTON_W, full.height())),
                        Rect::from_min_size(pos2(full.max.x - 3.0 * BUTTON_W, full.min.y), vec2(BUTTON_W, full.height())),
                    ];
                });
                window_edges(ui.ctx(), &c, edge, &mut ops);
            });
            out.textures_delta.clear();
            self.tab = tab;
            self.buttons = buttons;
            self.tab_clicks += clicks;
            ops
        }

        fn press(&mut self, pos: Pos2, pressed: bool, maximized: bool) -> Vec<WindowOp> {
            self.frame(
                vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }],
                maximized,
            )
        }

        fn click(&mut self, pos: Pos2, maximized: bool) -> Vec<WindowOp> {
            let mut ops = self.frame(vec![Event::PointerMoved(pos)], maximized);
            ops.extend(self.press(pos, true, maximized));
            ops.extend(self.press(pos, false, maximized));
            ops
        }
    }

    #[test]
    fn title_bar_moves_maximizes_and_closes_the_window() {
        let mut bar = Bar::new();
        bar.frame(vec![], false);
        let empty = pos2(600.0, 20.0);

        // Dragging the empty title bar moves the window.
        bar.frame(vec![Event::PointerMoved(empty)], false);
        let mut ops = bar.press(empty, true, false);
        for dx in 1..=10 {
            ops.extend(bar.frame(vec![Event::PointerMoved(empty + vec2(dx as f32 * 3.0, 0.0))], false));
        }
        assert_eq!(ops, vec![WindowOp::Drag]);
        bar.press(empty + vec2(30.0, 0.0), false, false);

        // A double click on it maximizes; one click does nothing.
        assert!(bar.click(empty, false).is_empty());
        assert_eq!(bar.click(empty, false), vec![WindowOp::ToggleMaximize]);

        // Tabs drawn on top keep their clicks.
        let tab = bar.tab.center();
        for _ in 0..30 {
            bar.frame(vec![], false); // long enough not to be a double click
        }
        assert!(bar.click(tab, false).is_empty());
        assert_eq!(bar.tab_clicks, 1);

        // The window buttons, right to left: close, maximize, minimize.
        let [close, size, minimize] = [bar.buttons[0].center(), bar.buttons[1].center(), bar.buttons[2].center()];
        assert_eq!(bar.click(minimize, false), vec![WindowOp::Minimize]);
        assert_eq!(bar.click(size, false), vec![WindowOp::ToggleMaximize]);
        assert_eq!(bar.click(close, false), vec![WindowOp::Close]);
    }

    #[test]
    fn edges_resize_except_when_maximized() {
        let mut bar = Bar::new();
        bar.frame(vec![], false);
        // The top band over the close button resizes instead of closing.
        let top_right = pos2(998.0, 1.0);
        bar.frame(vec![Event::PointerMoved(top_right)], false);
        assert_eq!(bar.press(top_right, true, false), vec![WindowOp::Resize(ResizeDirection::NorthEast)]);
        bar.press(top_right, false, false);
        let bottom = pos2(500.0, 598.0);
        bar.frame(vec![Event::PointerMoved(bottom)], false);
        assert_eq!(bar.press(bottom, true, false), vec![WindowOp::Resize(ResizeDirection::South)]);
        bar.press(bottom, false, false);
        // Maximized, the corner is the close button's.
        assert_eq!(bar.click(top_right, true), vec![WindowOp::Close]);
    }

    #[test]
    fn right_click_opens_the_window_menu() {
        let mut bar = Bar::new();
        bar.frame(vec![], false);
        let empty = pos2(600.0, 20.0);
        bar.frame(vec![Event::PointerMoved(empty)], false);
        let secondary = |pressed| Event::PointerButton { pos: empty, button: PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        bar.frame(vec![secondary(true)], false);
        bar.frame(vec![secondary(false)], false);
        bar.frame(vec![], false);
        assert!(egui::Popup::is_any_open(&bar.ctx), "the window menu is open");
    }
}
