//! Split view: two tabs side by side in the page area, as in Edge and
//! Chrome. Each tab keeps its own web view; the browser only places both.
//!
//! The pair is kept by tab id. The active tab is the focused pane: the
//! address bar, find, zoom and dev tools act on it. Switching to a tab
//! outside the pair shows that tab alone; switching back shows the pair.

/// The narrowest a pane may get, as a share of the page width.
pub const MIN_RATIO: f32 = 0.15;
pub const MAX_RATIO: f32 = 1.0 - MIN_RATIO;
/// The divider between the panes, in points (it is the drag handle).
pub const DIVIDER: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Split {
    pub left: u64,
    pub right: u64,
    /// The left pane's share of the width (without the divider).
    pub ratio: f32,
}

/// Keeps a ratio usable: within the limits, a broken value back to half.
pub fn clamp_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() { ratio.clamp(MIN_RATIO, MAX_RATIO) } else { 0.5 }
}

impl Split {
    /// A split of two different tabs, half and half.
    pub fn new(left: u64, right: u64) -> Option<Self> {
        (left != right).then_some(Split { left, right, ratio: 0.5 })
    }

    pub fn contains(&self, tab: u64) -> bool {
        tab == self.left || tab == self.right
    }

    pub fn side_of(&self, tab: u64) -> Option<Side> {
        if tab == self.left {
            Some(Side::Left)
        } else if tab == self.right {
            Some(Side::Right)
        } else {
            None
        }
    }

    /// The other tab of the pair.
    pub fn other(&self, tab: u64) -> Option<u64> {
        match self.side_of(tab)? {
            Side::Left => Some(self.right),
            Side::Right => Some(self.left),
        }
    }

    /// Swaps the panes (the ratio follows, so each pane keeps its width).
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.left, &mut self.right);
        self.ratio = clamp_ratio(1.0 - self.ratio);
    }

    pub fn set_ratio(&mut self, ratio: f32) {
        self.ratio = clamp_ratio(ratio);
    }

    /// The ratio for a divider dragged to `x`, in an area from `area_x`,
    /// `area_w` wide (any unit, the same for all three).
    pub fn ratio_at(x: f32, area_x: f32, area_w: f32, divider: f32) -> f32 {
        let usable = area_w - divider;
        if usable <= 1.0 {
            return 0.5;
        }
        clamp_ratio((x - area_x - divider / 2.0) / usable)
    }

    /// The horizontal spans `(x, width)` of the left pane, the divider and
    /// the right pane in an area `x`..`x + w`.
    pub fn spans(&self, x: f32, w: f32, divider: f32) -> [(f32, f32); 3] {
        let usable = (w - divider).max(2.0);
        let left_w = (usable * clamp_ratio(self.ratio)).round().max(1.0);
        let right_w = (usable - left_w).max(1.0);
        [(x, left_w), (x + left_w, divider), (x + left_w + divider, right_w)]
    }

    /// The span of `side`'s pane.
    pub fn pane(&self, side: Side, x: f32, w: f32, divider: f32) -> (f32, f32) {
        let [l, _, r] = self.spans(x, w, divider);
        match side {
            Side::Left => l,
            Side::Right => r,
        }
    }

    /// After tab `tab` closed: the split ends when it was one of the pair.
    pub fn after_close(self, tab: u64) -> Option<Split> {
        (!self.contains(tab)).then_some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_needs_two_tabs() {
        assert!(Split::new(3, 3).is_none());
        let s = Split::new(3, 7).unwrap();
        assert_eq!((s.left, s.right, s.ratio), (3, 7, 0.5));
        assert!(s.contains(7) && !s.contains(1));
        assert_eq!(s.other(3), Some(7));
        assert_eq!(s.other(7), Some(3));
        assert_eq!(s.other(1), None);
        assert_eq!(s.side_of(7), Some(Side::Right));
    }

    #[test]
    fn swap_keeps_pane_widths() {
        let mut s = Split::new(1, 2).unwrap();
        s.set_ratio(0.3);
        s.swap();
        assert_eq!((s.left, s.right), (2, 1));
        assert!((s.ratio - 0.7).abs() < 1e-6);
    }

    #[test]
    fn ratio_is_clamped() {
        let mut s = Split::new(1, 2).unwrap();
        s.set_ratio(0.0);
        assert_eq!(s.ratio, MIN_RATIO);
        s.set_ratio(2.0);
        assert_eq!(s.ratio, MAX_RATIO);
        s.set_ratio(f32::NAN);
        assert_eq!(s.ratio, 0.5);
        assert_eq!(Split::ratio_at(-50.0, 0.0, 1000.0, 8.0), MIN_RATIO);
        assert!((Split::ratio_at(504.0, 0.0, 1008.0, 8.0) - 0.5).abs() < 1e-6);
        assert_eq!(Split::ratio_at(5.0, 0.0, 4.0, 8.0), 0.5);
    }

    #[test]
    fn spans_fill_the_area() {
        let mut s = Split::new(1, 2).unwrap();
        s.set_ratio(0.25);
        let [l, d, r] = s.spans(100.0, 1008.0, 8.0);
        assert_eq!(l, (100.0, 250.0));
        assert_eq!(d, (350.0, 8.0));
        assert_eq!(r, (358.0, 750.0));
        assert_eq!(l.1 + d.1 + r.1, 1008.0);
        assert_eq!(s.pane(Side::Right, 100.0, 1008.0, 8.0), r);
    }

    #[test]
    fn closing_a_paired_tab_ends_the_split() {
        let s = Split::new(1, 2).unwrap();
        assert!(s.after_close(2).is_none());
        assert!(s.after_close(1).is_none());
        assert_eq!(s.after_close(5), Some(s));
    }
}
