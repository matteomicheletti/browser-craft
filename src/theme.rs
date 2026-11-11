//! Browser themes: a small set of colour tokens per theme, shared by the egui
//! chrome (`egui_chrome::Colors`), the software chrome and the internal pages
//! (`internal::themed`).
//!
//! Every colour the chrome paints comes from a [`Theme`]: chrome code never
//! names a colour itself. Identity colours (a container's colour, an
//! account's avatar) are the only exception.

/// Which theme the user picked. `System` follows the desktop's light / dark
/// preference with the [`LIGHT`] / [`DARK`] pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ThemeId {
    System,
    #[default]
    LateNight,
    SlateGray,
    Olive,
    Frost,
    Rose,
    Forest,
    Light,
    Sand,
}

impl ThemeId {
    /// Every choice, in the order the pickers list them.
    pub const ALL: [ThemeId; 9] = [
        ThemeId::System,
        ThemeId::LateNight,
        ThemeId::SlateGray,
        ThemeId::Olive,
        ThemeId::Frost,
        ThemeId::Rose,
        ThemeId::Forest,
        ThemeId::Light,
        ThemeId::Sand,
    ];

    /// The value stored in `flags.txt` and used in settings links.
    pub fn key(self) -> &'static str {
        match self {
            ThemeId::System => "system",
            ThemeId::LateNight => "late-night",
            ThemeId::SlateGray => "slate-gray",
            ThemeId::Olive => "olive",
            ThemeId::Frost => "frost",
            ThemeId::Rose => "rose",
            ThemeId::Forest => "forest",
            ThemeId::Light => "light",
            ThemeId::Sand => "sand",
        }
    }

    /// Parses a stored value. The values of the old light / dark switch
    /// (`auto`, `dark`) still work.
    pub fn from_key(key: &str) -> Option<ThemeId> {
        match key {
            "auto" => Some(ThemeId::System),
            "dark" => Some(ThemeId::LateNight),
            _ => ThemeId::ALL.into_iter().find(|t| t.key() == key),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeId::System => "Follow system",
            other => get(other).name,
        }
    }

    /// The theme this choice shows when the desktop is (or is not) dark.
    pub fn resolve(self, system_dark: bool) -> &'static Theme {
        match self {
            ThemeId::System if system_dark => &DARK,
            ThemeId::System => &LIGHT,
            other => get(other),
        }
    }
}

/// A theme's colours, as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    /// The window behind everything: title bar, tab strip, tab sidebar,
    /// window frame.
    pub background: u32,
    /// Toolbar, bars under it, menus, the palette.
    pub surface: u32,
    /// Hover, chips, notices, secondary buttons.
    pub surface2: u32,
    pub border: u32,
    pub text: u32,
    pub text_muted: u32,
    pub accent: u32,
    /// Text and icons drawn on `accent`.
    pub accent_text: u32,
    pub danger: u32,
    /// Text drawn on `danger`.
    pub danger_text: u32,
    pub warning: u32,
    pub success: u32,
    /// The selected tab (it joins the toolbar below it).
    pub tab_active: u32,
    /// The tab strip behind the other tabs.
    pub tab_inactive: u32,
    /// The address field and other text fields.
    pub address_bar: u32,
    /// The right-hand drawer.
    pub drawer: u32,
    /// Internal pages (settings, history, the new-tab page): the page and
    /// its cards.
    pub page: u32,
    pub card: u32,
}

/// A named set of tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub id: ThemeId,
    pub name: &'static str,
    /// A dark theme: web pages that follow the system preference, and the
    /// window's own theme, go dark with it.
    pub dark: bool,
    pub t: Tokens,
}

/// The two themes `ThemeId::System` uses.
pub const DARK: Theme = LATE_NIGHT;
pub const LIGHT: Theme = LIGHT_THEME;

const LATE_NIGHT: Theme = Theme {
    id: ThemeId::LateNight,
    name: "Late Night",
    dark: true,
    t: Tokens {
        background: 0x16181d,
        surface: 0x1b1e24,
        surface2: 0x2a2f38,
        border: 0x2c313a,
        text: 0xe8eaee,
        text_muted: 0x8b93a1,
        accent: 0x5b8def,
        accent_text: 0xffffff,
        danger: 0xe5484d,
        danger_text: 0xffffff,
        warning: 0xd99a1e,
        success: 0x30a46c,
        tab_active: 0x1b1e24,
        tab_inactive: 0x16181d,
        address_bar: 0x14161a,
        drawer: 0x16181d,
        page: 0x14161a,
        card: 0x1d2026,
    },
};

const SLATE_GRAY: Theme = Theme {
    id: ThemeId::SlateGray,
    name: "Slate Gray",
    dark: true,
    t: Tokens {
        background: 0x2a2f36,
        surface: 0x323840,
        surface2: 0x414852,
        border: 0x464f5a,
        text: 0xe6e9ed,
        text_muted: 0x9ba5b1,
        accent: 0x7fa7cc,
        accent_text: 0x101820,
        danger: 0xe5676b,
        danger_text: 0x1a0a0b,
        warning: 0xd9a54a,
        success: 0x5cb88a,
        tab_active: 0x323840,
        tab_inactive: 0x2a2f36,
        address_bar: 0x262a30,
        drawer: 0x2a2f36,
        page: 0x23272d,
        card: 0x2e343b,
    },
};

const OLIVE: Theme = Theme {
    id: ThemeId::Olive,
    name: "Olive",
    dark: true,
    t: Tokens {
        background: 0x22251a,
        surface: 0x2b2f21,
        surface2: 0x3a3f2d,
        border: 0x434933,
        text: 0xebecdc,
        text_muted: 0xa5a88e,
        accent: 0xa9bb63,
        accent_text: 0x1a1d0e,
        danger: 0xe0695a,
        danger_text: 0x1c0b08,
        warning: 0xd8a945,
        success: 0x8fbf5e,
        tab_active: 0x2b2f21,
        tab_inactive: 0x22251a,
        address_bar: 0x1c1f15,
        drawer: 0x22251a,
        page: 0x1c1f15,
        card: 0x2b2f21,
    },
};

const FROST: Theme = Theme {
    id: ThemeId::Frost,
    name: "Frost",
    dark: true,
    t: Tokens {
        background: 0x2e3440,
        surface: 0x3b4252,
        surface2: 0x434c5e,
        border: 0x4c566a,
        text: 0xeceff4,
        text_muted: 0xa3adc0,
        accent: 0x88c0d0,
        accent_text: 0x242933,
        danger: 0xbf616a,
        danger_text: 0xffffff,
        warning: 0xebcb8b,
        success: 0xa3be8c,
        tab_active: 0x3b4252,
        tab_inactive: 0x2e3440,
        address_bar: 0x2a2f3a,
        drawer: 0x2e3440,
        page: 0x2a2f3a,
        card: 0x3b4252,
    },
};

const ROSE: Theme = Theme {
    id: ThemeId::Rose,
    name: "Rosé",
    dark: true,
    t: Tokens {
        background: 0x191724,
        surface: 0x1f1d2e,
        surface2: 0x2a2740,
        border: 0x36334d,
        text: 0xe0def4,
        text_muted: 0x908caa,
        accent: 0xebbcba,
        accent_text: 0x191724,
        danger: 0xeb6f92,
        danger_text: 0x191724,
        warning: 0xf6c177,
        success: 0x9ccfd8,
        tab_active: 0x1f1d2e,
        tab_inactive: 0x191724,
        address_bar: 0x16141f,
        drawer: 0x191724,
        page: 0x16141f,
        card: 0x1f1d2e,
    },
};

const FOREST: Theme = Theme {
    id: ThemeId::Forest,
    name: "Forest",
    dark: true,
    t: Tokens {
        background: 0x122220,
        surface: 0x172b28,
        surface2: 0x223a36,
        border: 0x2a4540,
        text: 0xe2efec,
        text_muted: 0x8fb0a9,
        accent: 0x35b89e,
        accent_text: 0x05201a,
        danger: 0xe8685f,
        danger_text: 0x200806,
        warning: 0xe0b04a,
        success: 0x5cc98a,
        tab_active: 0x172b28,
        tab_inactive: 0x122220,
        address_bar: 0x0e1c1a,
        drawer: 0x122220,
        page: 0x0e1c1a,
        card: 0x172b28,
    },
};

const LIGHT_THEME: Theme = Theme {
    id: ThemeId::Light,
    name: "Light",
    dark: false,
    t: Tokens {
        background: 0xeceef2,
        surface: 0xf6f7f9,
        surface2: 0xe3e6ec,
        border: 0xdde1e7,
        text: 0x1c1f24,
        text_muted: 0x667085,
        accent: 0x2f6fde,
        accent_text: 0xffffff,
        danger: 0xd92d20,
        danger_text: 0xffffff,
        warning: 0xb7791f,
        success: 0x12a150,
        tab_active: 0xf6f7f9,
        tab_inactive: 0xeceef2,
        address_bar: 0xffffff,
        drawer: 0xeceef2,
        page: 0xf5f6f8,
        card: 0xffffff,
    },
};

const SAND: Theme = Theme {
    id: ThemeId::Sand,
    name: "Sand",
    dark: false,
    t: Tokens {
        background: 0xebe3d4,
        surface: 0xf6f1e7,
        surface2: 0xe3d8c4,
        border: 0xd8cab2,
        text: 0x2e2a24,
        text_muted: 0x7a6f5f,
        accent: 0xb3601c,
        accent_text: 0xffffff,
        danger: 0xc0392b,
        danger_text: 0xffffff,
        warning: 0x9c6510,
        success: 0x4d8a3a,
        tab_active: 0xf6f1e7,
        tab_inactive: 0xebe3d4,
        address_bar: 0xfffcf6,
        drawer: 0xebe3d4,
        page: 0xf3ede2,
        card: 0xfffaf1,
    },
};

/// Every concrete theme.
pub const THEMES: [&Theme; 8] = [&LATE_NIGHT, &SLATE_GRAY, &OLIVE, &FROST, &ROSE, &FOREST, &LIGHT_THEME, &SAND];

/// The theme of a concrete id (`System` gives the dark one).
pub fn get(id: ThemeId) -> &'static Theme {
    THEMES.into_iter().find(|t| t.id == id).unwrap_or(&DARK)
}

/// `a` blended towards `b` by `f` (0 = `a`, 1 = `b`).
pub fn mix(a: u32, b: u32, f: f32) -> u32 {
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((x + (y - x) * f.clamp(0.0, 1.0)).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// `#rrggbb`.
pub fn css_hex(c: u32) -> String {
    format!("#{:06x}", c & 0xff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance.
    fn luminance(c: u32) -> f32 {
        let lin = |v: u32| {
            let s = v as f32 / 255.0;
            if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin((c >> 16) & 0xff) + 0.7152 * lin((c >> 8) & 0xff) + 0.0722 * lin(c & 0xff)
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn keys_round_trip_and_old_values_still_load() {
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::from_key(id.key()), Some(id));
        }
        assert_eq!(ThemeId::from_key("auto"), Some(ThemeId::System));
        assert_eq!(ThemeId::from_key("dark"), Some(ThemeId::LateNight));
        assert_eq!(ThemeId::from_key("light"), Some(ThemeId::Light));
        assert_eq!(ThemeId::from_key("neon"), None);
    }

    #[test]
    fn every_id_but_system_has_its_own_theme() {
        assert_eq!(THEMES.len(), ThemeId::ALL.len() - 1);
        for id in ThemeId::ALL.into_iter().skip(1) {
            assert_eq!(get(id).id, id);
        }
        assert!(ThemeId::System.resolve(true).dark);
        assert!(!ThemeId::System.resolve(false).dark);
        assert_eq!(ThemeId::Sand.resolve(true).id, ThemeId::Sand, "a named theme ignores the desktop");
    }

    #[test]
    fn text_is_readable_on_every_theme() {
        for t in THEMES {
            let k = &t.t;
            for (what, fg, bg, min) in [
                ("text on surface", k.text, k.surface, 7.0),
                ("text on background", k.text, k.background, 7.0),
                ("text on address bar", k.text, k.address_bar, 7.0),
                ("muted on surface", k.text_muted, k.surface, 3.5),
                ("accent text on accent", k.accent_text, k.accent, 3.0),
                ("danger text on danger", k.danger_text, k.danger, 3.0),
                ("text on page", k.text, k.page, 7.0),
                ("text on card", k.text, k.card, 7.0),
            ] {
                let ratio = contrast(fg, bg);
                assert!(ratio >= min, "{}: {what} is {ratio:.2}:1", t.name);
            }
            assert_eq!(t.dark, luminance(k.background) < 0.2, "{} is marked by its background", t.name);
        }
    }

    #[test]
    fn mixing_and_css() {
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
        assert_eq!(mix(0x123456, 0xabcdef, 0.0), 0x123456);
        assert_eq!(css_hex(0x0a0b0c), "#0a0b0c");
    }
}
