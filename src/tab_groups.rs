//! Tab groups, as in Chrome and Edge: a named, coloured run of adjacent tabs
//! that can be collapsed to a single chip in the strip.
//!
//! The model knows tabs only by their stable id (`TabState::id`) and takes
//! the strip order (`&[u64]`, tab ids left to right) as an argument, so the
//! browser keeps owning the tabs and their order. Operations that move a tab
//! return the new order; the browser applies it. A group's tabs are always
//! adjacent (`normalize` restores that after any change).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The longest group name kept (Chrome has no limit; the chip truncates).
pub const MAX_NAME: usize = 64;
/// At most this many groups are read from a session file.
pub const MAX_GROUPS: usize = 200;

/// Chrome's group colours. The actual shade comes from the theme
/// (`egui_chrome::Colors::group_color`), so it fits light and dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupColor {
    #[default]
    Grey,
    Blue,
    Red,
    Yellow,
    Green,
    Pink,
    Purple,
    Cyan,
    Orange,
}

impl GroupColor {
    pub const ALL: [GroupColor; 9] = [
        GroupColor::Grey,
        GroupColor::Blue,
        GroupColor::Red,
        GroupColor::Yellow,
        GroupColor::Green,
        GroupColor::Pink,
        GroupColor::Purple,
        GroupColor::Cyan,
        GroupColor::Orange,
    ];

    pub fn name(self) -> &'static str {
        match self {
            GroupColor::Grey => "Grey",
            GroupColor::Blue => "Blue",
            GroupColor::Red => "Red",
            GroupColor::Yellow => "Yellow",
            GroupColor::Green => "Green",
            GroupColor::Pink => "Pink",
            GroupColor::Purple => "Purple",
            GroupColor::Cyan => "Cyan",
            GroupColor::Orange => "Orange",
        }
    }

    /// Light-theme and dark-theme shades (Chrome's), as `0xRRGGBB`.
    pub fn shades(self) -> (u32, u32) {
        match self {
            GroupColor::Grey => (0x5f6368, 0xbdc1c6),
            GroupColor::Blue => (0x1a73e8, 0x8ab4f8),
            GroupColor::Red => (0xd93025, 0xf28b82),
            GroupColor::Yellow => (0xe37400, 0xfdd663),
            GroupColor::Green => (0x1e8e3e, 0x81c995),
            GroupColor::Pink => (0xd01884, 0xff8bcb),
            GroupColor::Purple => (0x9334e6, 0xc58af9),
            GroupColor::Cyan => (0x007b83, 0x78d9ec),
            GroupColor::Orange => (0xc26401, 0xfcad70),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabGroup {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: GroupColor,
    #[serde(default)]
    pub collapsed: bool,
}

/// One entry of the strip or the sidebar, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripItem {
    /// A group's chip (or header row), before its tabs.
    Group(u64),
    /// The tab at this index (tabs of a collapsed group are left out).
    Tab(usize),
}

#[derive(Debug, Default)]
pub struct TabGroups {
    groups: Vec<TabGroup>,
    /// Tab id -> group id.
    member: HashMap<u64, u64>,
    next_id: u64,
}

fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim();
    match name.char_indices().nth(MAX_NAME) {
        Some((cut, _)) => name[..cut].to_string(),
        None => name.to_string(),
    }
}

impl TabGroups {
    pub fn all(&self) -> &[TabGroup] {
        &self.groups
    }

    pub fn get(&self, group: u64) -> Option<&TabGroup> {
        self.groups.iter().find(|g| g.id == group)
    }

    fn get_mut(&mut self, group: u64) -> Option<&mut TabGroup> {
        self.groups.iter_mut().find(|g| g.id == group)
    }

    /// The group tab `tab` is in.
    pub fn group_of(&self, tab: u64) -> Option<&TabGroup> {
        self.member.get(&tab).and_then(|g| self.get(*g))
    }

    /// The ids of `group`'s tabs, in strip order.
    pub fn members(&self, group: u64, order: &[u64]) -> Vec<u64> {
        order.iter().copied().filter(|t| self.member.get(t) == Some(&group)).collect()
    }

    /// A colour no group uses yet (Chrome's behaviour), else the next one
    /// round the wheel.
    fn next_color(&self) -> GroupColor {
        GroupColor::ALL
            .iter()
            .copied()
            .find(|c| !self.groups.iter().any(|g| g.color == *c))
            .unwrap_or(GroupColor::ALL[self.groups.len() % GroupColor::ALL.len()])
    }

    /// Creates an empty group (it lives once a tab joins it; empty groups
    /// are dropped by `normalize`).
    pub fn create(&mut self, name: &str, color: Option<GroupColor>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let color = color.unwrap_or_else(|| self.next_color());
        self.groups.push(TabGroup { id, name: clean_name(name), color, collapsed: false });
        id
    }

    pub fn rename(&mut self, group: u64, name: &str) -> bool {
        let name = clean_name(name);
        self.get_mut(group).map(|g| g.name = name).is_some()
    }

    pub fn set_color(&mut self, group: u64, color: GroupColor) -> bool {
        self.get_mut(group).map(|g| g.color = color).is_some()
    }

    pub fn set_collapsed(&mut self, group: u64, collapsed: bool) -> bool {
        self.get_mut(group).map(|g| g.collapsed = collapsed).is_some()
    }

    /// Puts `tab` in `group`, next to the group's other tabs (after the
    /// last one). Returns the new strip order.
    pub fn add_tab(&mut self, tab: u64, group: u64, order: &[u64]) -> Vec<u64> {
        if self.get(group).is_none() || !order.contains(&tab) {
            return order.to_vec();
        }
        let others = self.members(group, order);
        self.member.insert(tab, group);
        let mut out: Vec<u64> = order.to_vec();
        if let Some(last) = others.iter().rev().find(|t| **t != tab).copied() {
            out.retain(|t| *t != tab);
            let at = out.iter().position(|t| *t == last).map_or(out.len(), |p| p + 1);
            out.insert(at, tab);
        }
        self.normalize(&out)
    }

    /// Takes `tab` out of its group; it moves just after the group's run
    /// so the group stays in one piece. Returns the new strip order.
    pub fn remove_tab(&mut self, tab: u64, order: &[u64]) -> Vec<u64> {
        let Some(group) = self.member.remove(&tab) else {
            return order.to_vec();
        };
        let mut out: Vec<u64> = order.to_vec();
        let rest = self.members(group, order);
        if let Some(last) = rest.last().copied() {
            out.retain(|t| *t != tab);
            let at = out.iter().position(|t| *t == last).map_or(out.len(), |p| p + 1);
            out.insert(at, tab);
        }
        self.normalize(&out)
    }

    /// A tab closed: it leaves its group (an empty group goes).
    pub fn forget_tab(&mut self, tab: u64) {
        if let Some(group) = self.member.remove(&tab)
            && !self.member.values().any(|g| *g == group)
        {
            self.groups.retain(|g| g.id != group);
        }
    }

    /// Ungroups: the group goes, its tabs stay where they are.
    pub fn ungroup(&mut self, group: u64) {
        self.member.retain(|_, g| *g != group);
        self.groups.retain(|g| g.id != group);
    }

    /// Keeps each group's tabs adjacent (gathered where its first tab is),
    /// drops memberships of tabs that are gone and groups with no tab.
    /// Returns the order to apply.
    pub fn normalize(&mut self, order: &[u64]) -> Vec<u64> {
        self.member.retain(|t, _| order.contains(t));
        let known: Vec<u64> = self.groups.iter().map(|g| g.id).collect();
        self.member.retain(|_, g| known.contains(g));
        self.groups.retain(|g| self.member.values().any(|m| *m == g.id));
        let mut out = Vec::with_capacity(order.len());
        let mut placed: Vec<u64> = Vec::new();
        for tab in order {
            match self.member.get(tab) {
                None => out.push(*tab),
                Some(group) if !placed.contains(group) => {
                    placed.push(*group);
                    out.extend(self.members(*group, order));
                }
                Some(_) => {}
            }
        }
        out
    }

    /// What the strip shows, in order: each group's chip before its tabs,
    /// and the tabs of collapsed groups left out.
    pub fn strip(&self, order: &[u64]) -> Vec<StripItem> {
        let mut items = Vec::with_capacity(order.len());
        let mut current: Option<u64> = None;
        for (i, tab) in order.iter().enumerate() {
            let group = self.member.get(tab).copied();
            if group != current
                && let Some(g) = group
            {
                items.push(StripItem::Group(g));
            }
            current = group;
            let hidden = group.and_then(|g| self.get(g)).is_some_and(|g| g.collapsed);
            if !hidden {
                items.push(StripItem::Tab(i));
            }
        }
        items
    }

    /// Rebuilds the groups from a session file: `groups` as saved and, per
    /// restored tab (by position), the id of the group it was in.
    pub fn restore(&mut self, groups: Vec<TabGroup>, tabs: &[(u64, Option<u64>)]) -> Vec<u64> {
        self.groups.clear();
        self.member.clear();
        let mut map: HashMap<u64, u64> = HashMap::new();
        for saved in groups.into_iter().take(MAX_GROUPS) {
            if map.contains_key(&saved.id) {
                continue;
            }
            let id = self.create(&saved.name, Some(saved.color));
            self.set_collapsed(id, saved.collapsed);
            map.insert(saved.id, id);
        }
        for (tab, group) in tabs {
            if let Some(g) = group.and_then(|g| map.get(&g)) {
                self.member.insert(*tab, *g);
            }
        }
        let order: Vec<u64> = tabs.iter().map(|t| t.0).collect();
        self.normalize(&order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups_with(tabs: &[u64]) -> (TabGroups, u64, Vec<u64>) {
        let mut g = TabGroups::default();
        let order = vec![1, 2, 3, 4, 5];
        let id = g.create("Work", None);
        let mut order = order;
        for t in tabs {
            order = g.add_tab(*t, id, &order);
        }
        (g, id, order)
    }

    #[test]
    fn create_rename_recolour() {
        let mut g = TabGroups::default();
        let a = g.create("  Research\n ", None);
        let b = g.create("", None);
        assert_ne!(a, b);
        assert_eq!(g.get(a).map(|x| x.name.as_str()), Some("Research"));
        // New groups take a colour nobody uses.
        assert_ne!(g.get(a).map(|x| x.color), g.get(b).map(|x| x.color));
        assert!(g.rename(a, &"x".repeat(200)));
        assert_eq!(g.get(a).map(|x| x.name.chars().count()), Some(MAX_NAME));
        assert!(g.set_color(a, GroupColor::Pink));
        assert_eq!(g.get(a).map(|x| x.color), Some(GroupColor::Pink));
        assert!(!g.rename(99, "nope"));
    }

    #[test]
    fn adding_tabs_keeps_the_group_together() {
        let (g, id, order) = groups_with(&[2, 5]);
        // 5 moved next to 2.
        assert_eq!(order, vec![1, 2, 5, 3, 4]);
        assert_eq!(g.members(id, &order), vec![2, 5]);
        assert_eq!(g.group_of(5).map(|x| x.id), Some(id));
        assert!(g.group_of(3).is_none());
    }

    #[test]
    fn removing_a_tab_moves_it_after_the_group() {
        let (mut g, id, order) = groups_with(&[1, 2, 3]);
        let order = g.remove_tab(1, &order);
        assert_eq!(order, vec![2, 3, 1, 4, 5]);
        assert_eq!(g.members(id, &order), vec![2, 3]);
        // The last tab out: the group goes.
        let order = g.remove_tab(2, &order);
        let order = g.remove_tab(3, &order);
        assert!(g.get(id).is_none());
        assert_eq!(order.len(), 5);
    }

    #[test]
    fn collapse_hides_tabs_in_the_strip() {
        let (mut g, id, order) = groups_with(&[2, 3]);
        assert_eq!(
            g.strip(&order),
            vec![StripItem::Tab(0), StripItem::Group(id), StripItem::Tab(1), StripItem::Tab(2), StripItem::Tab(3), StripItem::Tab(4)]
        );
        g.set_collapsed(id, true);
        assert_eq!(g.strip(&order), vec![StripItem::Tab(0), StripItem::Group(id), StripItem::Tab(3), StripItem::Tab(4)]);
    }

    #[test]
    fn closing_and_ungrouping() {
        let (mut g, id, order) = groups_with(&[2, 3]);
        g.forget_tab(2);
        assert!(g.get(id).is_some());
        g.forget_tab(3);
        assert!(g.get(id).is_none());
        let (mut g, id, order2) = groups_with(&[4, 5]);
        g.ungroup(id);
        assert!(g.get(id).is_none() && g.group_of(4).is_none());
        assert_eq!(g.normalize(&order2), order2);
        let _ = order;
    }

    #[test]
    fn normalize_gathers_split_groups_and_drops_gone_tabs() {
        let (mut g, id, _) = groups_with(&[1, 3]);
        // Something moved tab 3 away from tab 1.
        let order = g.normalize(&[1, 2, 4, 3, 5]);
        assert_eq!(order, vec![1, 3, 2, 4, 5]);
        // Tabs that are gone leave the group.
        let order = g.normalize(&[2, 4, 5]);
        assert_eq!(order, vec![2, 4, 5]);
        assert!(g.get(id).is_none());
    }

    #[test]
    fn groups_survive_a_session_round_trip() {
        let (mut g, id, order) = groups_with(&[2, 3]);
        g.set_collapsed(id, true);
        g.set_color(id, GroupColor::Green);
        let saved: Vec<TabGroup> = g.all().to_vec();
        let json = serde_json::to_string(&saved).unwrap_or_default();
        let back: Vec<TabGroup> = serde_json::from_str(&json).unwrap_or_default();
        let tabs: Vec<(u64, Option<u64>)> = order.iter().map(|t| (t + 100, g.group_of(*t).map(|x| x.id))).collect();
        let mut r = TabGroups::default();
        let new_order = r.restore(back, &tabs);
        assert_eq!(new_order, vec![101, 102, 103, 104, 105]);
        let restored = r.group_of(102).cloned();
        assert_eq!(restored.as_ref().map(|x| (x.name.as_str(), x.color, x.collapsed)), Some(("Work", GroupColor::Green, true)));
        assert!(r.group_of(101).is_none());
        // Unknown colours and junk read defensively.
        let junk: Result<Vec<TabGroup>, _> = serde_json::from_str(r#"[{"id":1,"color":"mauve"}]"#);
        assert!(junk.is_err());
        let partial: Vec<TabGroup> = serde_json::from_str(r#"[{"id":1}]"#).unwrap_or_default();
        assert_eq!(partial.first().map(|x| x.color), Some(GroupColor::Grey));
    }
}
