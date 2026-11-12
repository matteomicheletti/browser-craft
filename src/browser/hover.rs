//! Tab hover cards, as in Chrome and Edge: resting the pointer on a tab in
//! the strip (or the sidebar) shows a card with the page's title, its site,
//! its container and group, and a picture of the page.
//!
//! Timing ([`HoverTimer`]): the first card appears after a delay (Chrome
//! waits about half a second; `hover-card-delay` on browsecraft://flags),
//! then moving to another tab swaps the card at once. Leaving the tabs
//! hides it; coming back within [`WARM`] shows the next one at once. A
//! click or a drag on a tab hides it until the pointer moves to another tab.
//!
//! Placement: the card hangs over the page, but the page is a native X11
//! window (WebKitGTK through wry) painted above everything egui draws, and
//! hiding the page while the card shows would flicker. So the card is its
//! own small X11 window, a child of the browser window like the web views
//! and raised above them (`x11::raise`), drawn on the CPU (softbuffer, the
//! engine's text and image painter). The X server stacks sibling windows
//! itself, so the card shows over the live page with nothing hidden or
//! redrawn underneath. The same kind of window can carry menus later.

use std::time::{Duration, Instant};

use crate::css::Color;
use crate::font::{FontKey, Fonts};
use crate::layout::Rect;
use crate::paint::{Canvas, Image};

/// After a card hides, the next one shows without the delay this long.
pub const WARM: Duration = Duration::from_millis(400);
/// The card's width in points.
pub const CARD_W: f32 = 264.0;

/// When to show a tab's hover card.
#[derive(Debug, Default)]
pub struct HoverTimer {
    state: State,
    warm_until: Option<Instant>,
    /// Clicked or dragged: no card for this tab until the pointer leaves it.
    suppressed: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Idle,
    /// Over a tab since `since`; the card shows after the delay.
    Pending { tab: u64, since: Instant },
    Shown { tab: u64 },
}

impl HoverTimer {
    /// The pointer is over tab `hovered` (or over no tab) at `now`. Returns
    /// the tab whose card shows now.
    pub fn update(&mut self, hovered: Option<u64>, now: Instant, delay: Duration) -> Option<u64> {
        if self.suppressed.is_some() && self.suppressed != hovered {
            self.suppressed = None;
        }
        let hovered = hovered.filter(|t| self.suppressed != Some(*t));
        self.state = match (hovered, self.state) {
            (None, State::Shown { .. }) => {
                self.warm_until = Some(now + WARM);
                State::Idle
            }
            (None, _) => State::Idle,
            (Some(tab), State::Shown { .. }) => State::Shown { tab },
            (Some(tab), State::Pending { since, .. }) => {
                if now.duration_since(since) >= delay {
                    State::Shown { tab }
                } else {
                    State::Pending { tab, since }
                }
            }
            (Some(tab), State::Idle) => {
                if delay.is_zero() || self.warm_until.is_some_and(|w| now < w) {
                    State::Shown { tab }
                } else {
                    State::Pending { tab, since: now }
                }
            }
        };
        self.shown()
    }

    /// The tab whose card shows.
    pub fn shown(&self) -> Option<u64> {
        match self.state {
            State::Shown { tab } => Some(tab),
            _ => None,
        }
    }

    /// When the pending card is due (to wake the event loop).
    pub fn due(&self, delay: Duration) -> Option<Instant> {
        match self.state {
            State::Pending { since, .. } => Some(since + delay),
            _ => None,
        }
    }

    /// A press on the tab: hide the card, no warm start.
    pub fn cancel(&mut self) {
        if let State::Shown { tab } | State::Pending { tab, .. } = self.state {
            self.suppressed = Some(tab);
        }
        self.state = State::Idle;
        self.warm_until = None;
    }
}

/// What a card shows.
#[derive(Clone, Debug, PartialEq)]
pub struct CardContent {
    pub tab: u64,
    pub title: String,
    /// The site (host), or the address of an internal page.
    pub host: String,
    /// Container name and colour (`0xRRGGBB`).
    pub container: Option<(String, u32)>,
    /// Group name and colour.
    pub group: Option<(String, u32)>,
    /// "Unloaded to save memory", "In split view", ...
    pub notes: Vec<String>,
    /// The picture's stamp (`thumbnails::Thumb::stamp`), when shown.
    pub thumb_stamp: Option<u64>,
}

/// Colours of a card, from the theme.
#[derive(Clone, Copy)]
pub struct CardColors {
    pub bg: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub image_bg: u32,
}

fn color(hex: u32) -> Color {
    Color { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 255 }
}

/// Fits `text` in `max_w` pixels, with an ellipsis when cut.
fn fit(fonts: &Fonts, key: FontKey, size: f32, text: &str, max_w: f32) -> String {
    if fonts.measure(key, text, size) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if fonts.measure(key, &format!("{out}\u{2026}"), size) > max_w {
            out.pop();
            break;
        }
    }
    format!("{}\u{2026}", out.trim_end())
}

/// Splits a title over at most two lines of `max_w` pixels.
fn two_lines(fonts: &Fonts, key: FontKey, size: f32, text: &str, max_w: f32) -> Vec<String> {
    if fonts.measure(key, text, size) <= max_w {
        return vec![text.to_string()];
    }
    let mut first = String::new();
    let mut rest = text;
    for (i, word) in text.split_inclusive(' ').enumerate() {
        let candidate = format!("{first}{word}");
        if fonts.measure(key, candidate.trim_end(), size) > max_w {
            if i == 0 {
                // One long word: cut it.
                return vec![fit(fonts, key, size, text, max_w)];
            }
            break;
        }
        first = candidate;
        rest = &text[first.len()..];
    }
    vec![first.trim_end().to_string(), fit(fonts, key, size, rest.trim(), max_w)]
}

/// The card's height in pixels for a picture of `image_h` pixels and
/// `title_lines` lines of title, at `scale`.
pub fn card_height(scale: f32, title_lines: usize, has_meta: bool, image_h: Option<u32>) -> u32 {
    let text = 12.0 + title_lines as f32 * 18.0 + 18.0 + if has_meta { 20.0 } else { 0.0 } + 10.0;
    (text * scale).round() as u32 + image_h.unwrap_or(0)
}

/// Draws a card (`CARD_W` points wide) into a new pixel buffer: the
/// title (two lines at most), the site, the container, group and notes,
/// and the picture under them. Returns `(width, height, 0RGB pixels)`.
pub fn render_card(fonts: &Fonts, content: &CardContent, image: Option<&Image>, c: CardColors, scale: f32) -> (u32, u32, Vec<u32>) {
    let s = scale.max(0.5);
    let w = (CARD_W * s).round() as u32;
    let pad = 12.0 * s;
    let inner = w as f32 - 2.0 * pad;
    let bold = FontKey { bold: true, ..FontKey::UI };
    let title = if content.title.trim().is_empty() { "Untitled" } else { content.title.trim() };
    let lines = two_lines(fonts, bold, 13.0 * s, title, inner);
    let mut meta: Vec<(String, u32)> = Vec::new();
    if let Some((name, rgb)) = &content.container {
        meta.push((name.clone(), *rgb));
    }
    if let Some((name, rgb)) = &content.group {
        meta.push((if name.is_empty() { "Group".to_string() } else { name.clone() }, *rgb));
    }
    let has_meta = !meta.is_empty() || !content.notes.is_empty();
    let image_h = image.map(|img| ((img.height as f32) * (w as f32 / img.width.max(1) as f32)).round() as u32);
    let h = card_height(s, lines.len(), has_meta, image_h);
    let mut px = vec![c.bg; (w * h) as usize];
    {
        let mut canvas = Canvas::new(&mut px, w as usize, h as usize);
        let mut y = 12.0 * s;
        for line in &lines {
            let row = Rect { x: pad, y, w: inner, h: 18.0 * s };
            let (ascent, descent) = fonts.vertical_metrics(bold, 13.0 * s);
            let baseline = row.y + (row.h - (ascent + descent)) / 2.0 + ascent;
            canvas.draw_text(fonts, pad, baseline, line, bold, 13.0 * s, color(c.text));
            y += 18.0 * s;
        }
        let host = fit(fonts, FontKey::UI, 12.0 * s, &content.host, inner);
        let (ascent, descent) = fonts.vertical_metrics(FontKey::UI, 12.0 * s);
        let baseline = y + (18.0 * s - (ascent + descent)) / 2.0 + ascent;
        canvas.draw_text(fonts, pad, baseline, &host, FontKey::UI, 12.0 * s, color(c.muted));
        y += 18.0 * s;
        if has_meta {
            let mut x = pad;
            let baseline = y + (20.0 * s - (ascent + descent)) / 2.0 + ascent;
            for (name, rgb) in &meta {
                let dot = 8.0 * s;
                canvas.fill_rounded(Rect { x, y: y + (20.0 * s - dot) / 2.0, w: dot, h: dot }, dot / 2.0, color(*rgb), None);
                x += dot + 5.0 * s;
                let text = fit(fonts, FontKey::UI, 11.5 * s, name, (pad + inner - x).max(0.0));
                canvas.draw_text(fonts, x, baseline, &text, FontKey::UI, 11.5 * s, color(c.text));
                x += fonts.measure(FontKey::UI, &text, 11.5 * s) + 12.0 * s;
            }
            for note in &content.notes {
                if x >= pad + inner {
                    break;
                }
                let text = fit(fonts, FontKey::UI, 11.5 * s, note, (pad + inner - x).max(0.0));
                canvas.draw_text(fonts, x, baseline, &text, FontKey::UI, 11.5 * s, color(c.muted));
                x += fonts.measure(FontKey::UI, &text, 11.5 * s) + 12.0 * s;
            }
            y += 20.0 * s;
        }
        y += 10.0 * s;
        if let (Some(img), Some(ih)) = (image, image_h) {
            let r = Rect { x: 0.0, y, w: w as f32, h: ih as f32 };
            canvas.fill_rect(r, color(c.image_bg));
            canvas.draw_image(img, r);
            canvas.fill_rect(Rect { x: 0.0, y, w: w as f32, h: 1.0 }, color(c.border));
        }
        // A one-pixel frame round the card.
        let (fw, fh) = (w as f32, h as f32);
        for r in [
            Rect { x: 0.0, y: 0.0, w: fw, h: 1.0 },
            Rect { x: 0.0, y: fh - 1.0, w: fw, h: 1.0 },
            Rect { x: 0.0, y: 0.0, w: 1.0, h: fh },
            Rect { x: fw - 1.0, y: 0.0, w: 1.0, h: fh },
        ] {
            canvas.fill_rect(r, color(c.border));
        }
    }
    (w, h, px)
}

/// A page picture scaled to `width` pixels (good quality), for the card.
pub fn scaled_image(thumb: &crate::thumbnails::Thumb, width: u32) -> Option<Image> {
    let source = image::RgbaImage::from_raw(thumb.w as u32, thumb.h as u32, thumb.rgba.to_vec())?;
    let height = ((thumb.h as f32) * (width as f32 / thumb.w.max(1) as f32)).round().max(1.0) as u32;
    let scaled = image::imageops::resize(&source, width.max(1), height, image::imageops::FilterType::Triangle);
    let pixels = scaled
        .pixels()
        .map(|p| (u32::from(p[3]) << 24) | (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
        .collect();
    Some(Image { width: scaled.width(), height: scaled.height(), pixels })
}

/// Where a card goes, in the parent window's pixels: under the tab's chip,
/// kept inside the window.
pub fn card_position(anchor: Rect, card: (u32, u32), window: (u32, u32), gap: f32) -> (i32, i32) {
    let (cw, ch) = (card.0 as f32, card.1 as f32);
    let (ww, wh) = (window.0 as f32, window.1 as f32);
    let (x, y) = (anchor.x, anchor.y + anchor.h + gap);
    let x = x.min(ww - cw - 2.0).max(2.0);
    let y = y.min(wh - ch - 2.0).max(2.0);
    (x.round() as i32, y.round() as i32)
}

/// The card's own window: an X11 child of the browser window, raised above
/// the web views, drawn with softbuffer.
#[cfg(target_os = "linux")]
pub struct CardWindow {
    pub window: std::sync::Arc<winit::window::Window>,
    surface: softbuffer::Surface<std::sync::Arc<winit::window::Window>, std::sync::Arc<winit::window::Window>>,
    _context: softbuffer::Context<std::sync::Arc<winit::window::Window>>,
    /// What it shows and where, as last drawn.
    pub shown: Option<(CardContent, (i32, i32))>,
    pub visible: bool,
    /// The picture scaled for the card: (tab, stamp, width) and pixels.
    image: Option<((u64, u64, u32), Image)>,
    /// The last frame, to draw again on expose.
    last: Option<(u32, u32, Vec<u32>)>,
    /// What the last frame was drawn from (content, anchor, placement,
    /// colours, scale, window size), to skip drawing it again.
    drawn: Option<String>,
}

#[cfg(target_os = "linux")]
impl CardWindow {
    /// Creates the (hidden) card window inside `parent`.
    pub fn new(event_loop: &winit::event_loop::ActiveEventLoop, parent: &winit::window::Window) -> Option<Self> {
        use winit::raw_window_handle::HasWindowHandle as _;
        let handle = parent.window_handle().ok()?.as_raw();
        let attrs = winit::window::Window::default_attributes()
            .with_title("BrowseCraft tab preview")
            .with_visible(false)
            .with_decorations(false)
            .with_inner_size(winit::dpi::PhysicalSize::new(10u32, 10u32))
            .with_position(winit::dpi::PhysicalPosition::new(0, 0));
        // SAFETY: `parent` outlives the card (the browser drops the card
        // first, see `Browser`'s `Drop`).
        let attrs = unsafe { attrs.with_parent_window(Some(handle)) };
        let window = std::sync::Arc::new(event_loop.create_window(attrs).ok()?);
        let context = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&context, window.clone()).ok()?;
        Some(CardWindow { window, surface, _context: context, shown: None, visible: false, image: None, last: None, drawn: None })
    }

    /// Shows `content` at `anchor` (parent pixels). Draws only when it
    /// changed; raises the window above the web views every time.
    #[allow(clippy::too_many_arguments)]
    pub fn show(&mut self, fonts: &Fonts, content: CardContent, thumb: Option<&crate::thumbnails::Thumb>, anchor: Rect, c: CardColors, scale: f32, window_size: (u32, u32)) {
        let key = format!(
            "{content:?}|{:?}|{:?}|{scale}|{window_size:?}",
            (anchor.x, anchor.y, anchor.w, anchor.h),
            (c.bg, c.border, c.text, c.muted, c.image_bg)
        );
        if self.visible && self.drawn.as_deref() == Some(key.as_str()) {
            return;
        }
        let width = (CARD_W * scale).round() as u32;
        let image = match thumb {
            Some(t) => {
                let key = (content.tab, t.stamp, width);
                if self.image.as_ref().is_none_or(|(k, _)| *k != key) {
                    self.image = scaled_image(t, width).map(|img| (key, img));
                }
                self.image.as_ref().map(|(_, img)| img)
            }
            None => None,
        };
        let (w, h, pixels) = render_card(fonts, &content, image, c, scale);
        let pos = card_position(anchor, (w, h), window_size, 6.0 * scale);
        let same = self.shown.as_ref().is_some_and(|(old, at)| *old == content && *at == pos);
        if !same || !self.visible {
            let _ = self.window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
            self.window.set_outer_position(winit::dpi::PhysicalPosition::new(pos.0, pos.1));
            self.present(w, h, &pixels);
            self.shown = Some((content, pos));
        }
        if !self.visible {
            self.window.set_visible(true);
            self.visible = true;
        }
        // GTK raises a web view's window when it shows it: on top again.
        super::x11::raise(&self.window);
        // The window may be mapped or resized after this: draw again then.
        self.last = Some((w, h, pixels));
        self.drawn = Some(key);
    }

    /// Paints `pixels` (`w × h`, 0RGB) into the window.
    fn present(&mut self, w: u32, h: u32, pixels: &[u32]) {
        let (Some(nw), Some(nh)) = (std::num::NonZeroU32::new(w), std::num::NonZeroU32::new(h)) else { return };
        if self.surface.resize(nw, nh).is_err() {
            return;
        }
        if let Ok(mut buffer) = self.surface.buffer_mut() {
            let n = buffer.len().min(pixels.len());
            buffer[..n].copy_from_slice(&pixels[..n]);
            let _ = buffer.present();
        }
    }

    /// Draws the last card again (the window was exposed or resized).
    pub fn redraw(&mut self) {
        if let Some((w, h, pixels)) = self.last.take() {
            self.present(w, h, &pixels);
            self.last = Some((w, h, pixels));
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.window.set_visible(false);
            self.visible = false;
            self.drawn = None;
        }
    }
}

/// The card on macOS: a native view above the web views (they are sibling
/// views of the browser window's view), drawn with softbuffer like on Linux.
#[cfg(target_os = "macos")]
pub struct CardWindow {
    view: crate::macos_web::CardView,
    surface: softbuffer::Surface<crate::macos_web::ViewHandle, crate::macos_web::ViewHandle>,
    _context: softbuffer::Context<crate::macos_web::ViewHandle>,
    pub shown: Option<(CardContent, (i32, i32))>,
    pub visible: bool,
    /// The picture scaled for the card: (tab, stamp, width) and pixels.
    image: Option<((u64, u64, u32), Image)>,
    drawn: Option<String>,
}

#[cfg(target_os = "macos")]
impl CardWindow {
    pub fn new(_event_loop: &winit::event_loop::ActiveEventLoop, parent: &winit::window::Window) -> Option<Self> {
        let (view, handle) = crate::macos_web::CardView::new(parent)?;
        let context = softbuffer::Context::new(handle.clone()).ok()?;
        let surface = softbuffer::Surface::new(&context, handle).ok()?;
        Some(CardWindow { view, surface, _context: context, shown: None, visible: false, image: None, drawn: None })
    }

    /// Shows `content` at `anchor` (window pixels). Draws only when it changed.
    #[allow(clippy::too_many_arguments)]
    pub fn show(&mut self, fonts: &Fonts, content: CardContent, thumb: Option<&crate::thumbnails::Thumb>, anchor: Rect, c: CardColors, scale: f32, window_size: (u32, u32)) {
        let key = format!(
            "{content:?}|{:?}|{:?}|{scale}|{window_size:?}",
            (anchor.x, anchor.y, anchor.w, anchor.h),
            (c.bg, c.border, c.text, c.muted, c.image_bg)
        );
        if self.visible && self.drawn.as_deref() == Some(key.as_str()) {
            return;
        }
        let width = (CARD_W * scale).round() as u32;
        let image = match thumb {
            Some(t) => {
                let key = (content.tab, t.stamp, width);
                if self.image.as_ref().is_none_or(|(k, _)| *k != key) {
                    self.image = scaled_image(t, width).map(|img| (key, img));
                }
                self.image.as_ref().map(|(_, img)| img)
            }
            None => None,
        };
        let (w, h, pixels) = render_card(fonts, &content, image, c, scale);
        let pos = card_position(anchor, (w, h), window_size, 6.0 * scale);
        self.view.place(pos, w, h, scale);
        if let (Some(nw), Some(nh)) = (std::num::NonZeroU32::new(w), std::num::NonZeroU32::new(h))
            && self.surface.resize(nw, nh).is_ok()
            && let Ok(mut buffer) = self.surface.buffer_mut()
        {
            let n = buffer.len().min(pixels.len());
            buffer[..n].copy_from_slice(&pixels[..n]);
            let _ = buffer.present();
        }
        self.shown = Some((content, pos));
        if !self.visible {
            self.view.raise();
            self.view.set_visible(true);
            self.visible = true;
        }
        self.drawn = Some(key);
    }

    pub fn raise(&self) {
        self.view.raise();
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.view.set_visible(false);
            self.visible = false;
            self.drawn = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELAY: Duration = Duration::from_millis(500);

    #[test]
    fn the_first_card_waits_for_the_delay() {
        let t0 = Instant::now();
        let mut h = HoverTimer::default();
        assert_eq!(h.update(Some(1), t0, DELAY), None);
        assert_eq!(h.due(DELAY), Some(t0 + DELAY));
        assert_eq!(h.update(Some(1), t0 + Duration::from_millis(200), DELAY), None);
        assert_eq!(h.update(Some(1), t0 + DELAY, DELAY), Some(1));
    }

    #[test]
    fn moving_between_tabs_swaps_the_card_at_once() {
        let t0 = Instant::now();
        let mut h = HoverTimer::default();
        h.update(Some(1), t0, DELAY);
        // Moving before the card showed keeps the timer running.
        assert_eq!(h.update(Some(2), t0 + Duration::from_millis(300), DELAY), None);
        assert_eq!(h.update(Some(2), t0 + DELAY, DELAY), Some(2));
        assert_eq!(h.update(Some(3), t0 + DELAY + Duration::from_millis(10), DELAY), Some(3));
    }

    #[test]
    fn leaving_hides_and_a_quick_return_is_immediate() {
        let t0 = Instant::now();
        let mut h = HoverTimer::default();
        h.update(Some(1), t0, DELAY);
        h.update(Some(1), t0 + DELAY, DELAY);
        let left = t0 + DELAY + Duration::from_millis(50);
        assert_eq!(h.update(None, left, DELAY), None);
        assert_eq!(h.update(Some(2), left + Duration::from_millis(100), DELAY), Some(2), "within the warm time");
        h.update(None, left + Duration::from_millis(200), DELAY);
        let later = left + Duration::from_secs(2);
        assert_eq!(h.update(Some(2), later, DELAY), None, "cold again: wait");
    }

    #[test]
    fn a_click_hides_the_card_until_another_tab() {
        let t0 = Instant::now();
        let mut h = HoverTimer::default();
        h.update(Some(1), t0, DELAY);
        h.update(Some(1), t0 + DELAY, DELAY);
        h.cancel();
        assert_eq!(h.update(Some(1), t0 + Duration::from_secs(3), DELAY), None);
        assert_eq!(h.due(DELAY), None, "no timer for the clicked tab");
        let t1 = t0 + Duration::from_secs(4);
        assert_eq!(h.update(Some(2), t1, DELAY), None);
        assert_eq!(h.update(Some(2), t1 + DELAY, DELAY), Some(2));
    }

    #[test]
    fn no_delay_shows_at_once() {
        let mut h = HoverTimer::default();
        assert_eq!(h.update(Some(4), Instant::now(), Duration::ZERO), Some(4));
    }

    #[test]
    fn cards_stay_inside_the_window() {
        let anchor = Rect { x: 900.0, y: 0.0, w: 200.0, h: 38.0 };
        // Under the chip, pushed left at the right edge.
        assert_eq!(card_position(anchor, (264, 200), (1000, 800), 6.0), (734, 44));
        // Pushed up at the bottom edge.
        let low = Rect { x: 10.0, y: 700.0, w: 200.0, h: 34.0 };
        assert_eq!(card_position(low, (264, 200), (1000, 800), 6.0), (10, 598));
    }

    #[test]
    fn card_height_counts_the_picture() {
        assert_eq!(card_height(1.0, 1, false, None), 58);
        assert_eq!(card_height(1.0, 2, true, Some(150)), 58 + 18 + 20 + 150);
        assert_eq!(card_height(2.0, 1, false, None), 116);
    }
}

// ---- the window side ----

impl super::Browser {
    /// A web view was just shown (GTK raised it): the card on top again.
    pub(super) fn raise_hover_card(&self) {
        #[cfg(target_os = "linux")]
        if let Some(card) = self.card.as_ref().filter(|c| c.visible) {
            super::x11::raise(&card.window);
        }
        #[cfg(target_os = "macos")]
        if let Some(card) = self.card.as_ref().filter(|c| c.visible) {
            card.raise();
        }
    }

    pub(super) fn hide_hover_card(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(card) = &mut self.card {
            card.hide();
        }
    }

    /// After a frame: shows, moves or hides the hover card for the tab
    /// under the pointer (`hover_anchor`).
    pub(super) fn update_hover_card(&mut self, ppp: f32) {
        let f = crate::flags::get();
        // Not gated on the window's focus: winit reports the window as
        // unfocused while the keyboard is in the page (a child window).
        let blocked = !f.hover_cards || self.egui_popup_open || self.overview.open || self.strip_drag.is_some() || self.web_fullscreen;
        let hovered = if blocked { None } else { self.hover_anchor.map(|(id, _)| id) };
        let delay = std::time::Duration::from_millis(u64::from(f.hover_card_delay_ms));
        let shown = self.hover.update(hovered, Instant::now(), delay);
        let (Some(id), Some((_, rect))) = (shown, self.hover_anchor) else {
            self.hide_hover_card();
            return;
        };
        let Some(i) = self.tab_index(id) else {
            self.hide_hover_card();
            return;
        };
        let colors = crate::egui_chrome::Colors::from_theme(self.theme());
        let url = self.tab_url(i);
        let host = url::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()))
            .unwrap_or_else(|| url.clone());
        let rgb = |c: egui::Color32| (u32::from(c.r()) << 16) | (u32::from(c.g()) << 8) | u32::from(c.b());
        let container = self.tab_container(i).and_then(|c| crate::containers::get(&c)).map(|c| (c.name.clone(), rgb(c.color32())));
        let group = self.groups.group_of(id).map(|g| (g.name.clone(), rgb(colors.group_color(g.color))));
        let mut notes = Vec::new();
        if self.tab_unloaded(i) {
            notes.push("Unloaded to save memory".to_string());
        }
        if self.split.is_some_and(|s| s.contains(id)) {
            notes.push("Split view".to_string());
        }
        // The active tab is on screen: no picture (as in Chrome).
        let thumb = (f.hover_card_images && i != self.active).then(|| self.shared.borrow().thumbs.peek(id).cloned()).flatten();
        let content = CardContent {
            tab: id,
            title: self.tab_title(i),
            host,
            container,
            group,
            notes,
            thumb_stamp: thumb.as_ref().map(|t| t.stamp),
        };
        let t = &self.theme().t;
        let card_colors = CardColors { bg: t.surface, border: t.border, text: t.text, muted: t.text_muted, image_bg: t.surface2 };
        let anchor = Rect { x: rect.min.x * ppp, y: rect.min.y * ppp, w: rect.width() * ppp, h: rect.height() * ppp };
        let size = self.gfx.as_ref().map(|g| g.window.inner_size()).map(|s| (s.width, s.height)).unwrap_or((1, 1));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(card) = &mut self.card {
            card.show(&self.fonts, content, thumb.as_ref(), anchor, card_colors, ppp, size);
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = (content, card_colors, anchor, size, thumb);
    }
}
