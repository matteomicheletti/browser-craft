//! Every browser action the command palette can run.
//!
//! The palette only picks a [`CommandId`]; the browser implements
//! `run_command(&mut self, id: CommandId)`. Keeping the catalog as plain data
//! lets the palette search it, show the shortcut and grey out the commands
//! that need a web page.
// The API is wired into the browser in the integration step.
#![allow(dead_code)]

use egui_phosphor::regular as ph;

/// What the browser can do. Add a variant here, one catalog row below, and a
/// match arm in the browser's `run_command`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandId {
    // Tabs
    NewTab,
    NewTabInContainer,
    NewTemporaryContainerTab,
    DuplicateTab,
    CloseTab,
    CloseOtherTabs,
    ReopenClosedTab,
    NextTab,
    PrevTab,
    ToggleTabsPosition,
    // Tab groups and split view
    NewTabGroup,
    AddTabToGroup,
    RemoveTabFromGroup,
    ToggleGroupCollapsed,
    SplitWithTab,
    SwapSplitPanes,
    FocusOtherPane,
    CloseSplitView,
    // Navigation
    Back,
    Forward,
    Reload,
    HardReload,
    Home,
    FocusAddress,
    CopyUrl,
    // Page
    Find,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Fullscreen,
    Print,
    SavePage,
    ViewSource,
    // Capture
    Screenshot,
    ScreenshotFullPage,
    StartRecording,
    StopRecording,
    // Developer
    DevTools,
    JsConsole,
    AutomationToggle,
    // Bookmarks and history
    Bookmark,
    BookmarksBar,
    BookmarksPage,
    History,
    ClearBrowsingData,
    // Privacy and accounts
    Containers,
    Authenticator,
    Passwords,
    // Appearance and extensions
    Theme,
    DarkModeToggle,
    DarkModeSiteToggle,
    DarkModeSettings,
    Plugins,
    ReloadPlugins,
    // Browser
    Settings,
    Quit,
}

/// Group shown next to the command in the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Tabs,
    Navigation,
    Page,
    Capture,
    Developer,
    Library,
    Privacy,
    Appearance,
    Browser,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Tabs => "Tabs",
            Category::Navigation => "Navigation",
            Category::Page => "Page",
            Category::Capture => "Capture",
            Category::Developer => "Developer",
            Category::Library => "Library",
            Category::Privacy => "Privacy",
            Category::Appearance => "Appearance",
            Category::Browser => "Browser",
        }
    }
}

/// One catalog row.
#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub id: CommandId,
    /// Stable name used to persist recent commands. Never rename.
    pub key: &'static str,
    pub label: &'static str,
    /// Extra words the fuzzy search matches besides the label.
    pub keywords: &'static [&'static str],
    /// Chrome-style text with `Ctrl`; see [`display_shortcut`].
    pub shortcut: Option<&'static str>,
    /// A Phosphor glyph.
    pub icon: &'static str,
    pub category: Category,
}

impl Command {
    /// True for commands that act on the current web page, which the palette
    /// greys out on internal pages and empty tabs.
    pub fn needs_page(&self) -> bool {
        needs_page(self.id)
    }
}

/// Whether `id` acts on the current web page.
pub fn needs_page(id: CommandId) -> bool {
    use CommandId::*;
    matches!(
        id,
        Reload
            | HardReload
            | CopyUrl
            | Find
            | ZoomIn
            | ZoomOut
            | ZoomReset
            | Print
            | SavePage
            | ViewSource
            | Screenshot
            | ScreenshotFullPage
            | DevTools
            | JsConsole
            | Bookmark
            | DarkModeSiteToggle
            | DuplicateTab
    )
}

/// The shortcut as the user's platform writes it: `Ctrl` becomes `Cmd` on
/// macOS.
pub fn display_shortcut(text: &str) -> String {
    if cfg!(target_os = "macos") {
        text.replace("Ctrl", "Cmd")
    } else {
        text.to_string()
    }
}

/// The command with this persisted key.
pub fn from_key(key: &str) -> Option<CommandId> {
    catalog().iter().find(|c| c.key == key).map(|c| c.id)
}

/// The catalog row of `id`.
pub fn get(id: CommandId) -> Option<&'static Command> {
    catalog().iter().find(|c| c.id == id)
}

macro_rules! cmd {
    ($id:ident, $key:expr, $label:expr, [$($kw:expr),*], $sc:expr, $icon:expr, $cat:ident) => {
        Command {
            id: CommandId::$id,
            key: $key,
            label: $label,
            keywords: &[$($kw),*],
            shortcut: $sc,
            icon: $icon,
            category: Category::$cat,
        }
    };
}

/// All commands, in the order shown when the search box is empty.
pub fn catalog() -> &'static [Command] {
    &CATALOG
}

static CATALOG: [Command; 56] = [
    cmd!(
        NewTab,
        "new-tab",
        "New tab",
        ["open", "blank"],
        Some("Ctrl+T"),
        ph::PLUS,
        Tabs
    ),
    cmd!(
        NewTabInContainer,
        "new-tab-container",
        "New tab in container…",
        ["identity", "profile", "container"],
        None,
        ph::SQUARES_FOUR,
        Tabs
    ),
    cmd!(
        NewTemporaryContainerTab,
        "new-tab-temporary",
        "New temporary container tab",
        ["private", "incognito", "ephemeral", "throwaway"],
        Some("Ctrl+Shift+N"),
        ph::GHOST,
        Tabs
    ),
    cmd!(
        DuplicateTab,
        "duplicate-tab",
        "Duplicate tab",
        ["clone", "copy"],
        None,
        ph::COPY,
        Tabs
    ),
    cmd!(
        CloseTab,
        "close-tab",
        "Close tab",
        ["quit tab", "remove"],
        Some("Ctrl+W"),
        ph::X,
        Tabs
    ),
    cmd!(
        CloseOtherTabs,
        "close-other-tabs",
        "Close other tabs",
        ["all but this"],
        None,
        ph::X_SQUARE,
        Tabs
    ),
    cmd!(
        ReopenClosedTab,
        "reopen-tab",
        "Reopen closed tab",
        ["undo close", "restore"],
        Some("Ctrl+Shift+T"),
        ph::ARROW_COUNTER_CLOCKWISE,
        Tabs
    ),
    cmd!(
        NextTab,
        "next-tab",
        "Next tab",
        ["switch", "right"],
        Some("Ctrl+Tab"),
        ph::CARET_RIGHT,
        Tabs
    ),
    cmd!(
        PrevTab,
        "prev-tab",
        "Previous tab",
        ["switch", "left"],
        Some("Ctrl+Shift+Tab"),
        ph::CARET_LEFT,
        Tabs
    ),
    cmd!(
        ToggleTabsPosition,
        "toggle-tabs-position",
        "Move tabs to top or side",
        ["vertical", "horizontal", "sidebar", "layout"],
        None,
        ph::SIDEBAR_SIMPLE,
        Tabs
    ),
    cmd!(
        NewTabGroup,
        "group-new",
        "New tab group",
        ["group", "folder", "organize", "add tab to new group"],
        None,
        ph::FOLDER_SIMPLE_PLUS,
        Tabs
    ),
    cmd!(
        AddTabToGroup,
        "group-add",
        "Add tab to group…",
        ["group", "move", "folder"],
        None,
        ph::FOLDER_SIMPLE,
        Tabs
    ),
    cmd!(
        RemoveTabFromGroup,
        "group-remove",
        "Remove tab from group",
        ["ungroup", "folder"],
        None,
        ph::FOLDER_SIMPLE_MINUS,
        Tabs
    ),
    cmd!(
        ToggleGroupCollapsed,
        "group-collapse",
        "Collapse or expand tab group",
        ["fold", "hide", "show", "group"],
        None,
        ph::CARET_UP_DOWN,
        Tabs
    ),
    cmd!(
        SplitWithTab,
        "split-with",
        "Split view with tab…",
        ["side by side", "two tabs", "split screen", "compare"],
        None,
        ph::COLUMNS,
        Tabs
    ),
    cmd!(
        SwapSplitPanes,
        "split-swap",
        "Swap split view panes",
        ["side by side", "left right", "split screen"],
        None,
        ph::ARROWS_LEFT_RIGHT,
        Tabs
    ),
    cmd!(
        FocusOtherPane,
        "split-focus-other",
        "Focus the other split pane",
        ["side by side", "switch pane", "split screen"],
        None,
        ph::ARROW_SQUARE_RIGHT,
        Tabs
    ),
    cmd!(
        CloseSplitView,
        "split-close",
        "Close split view",
        ["side by side", "single", "split screen", "unsplit"],
        None,
        ph::SQUARE_SPLIT_HORIZONTAL,
        Tabs
    ),
    cmd!(
        Back,
        "back",
        "Back",
        ["previous page", "history"],
        Some("Alt+Left"),
        crate::icons::BACK,
        Navigation
    ),
    cmd!(
        Forward,
        "forward",
        "Forward",
        ["next page"],
        Some("Alt+Right"),
        crate::icons::FORWARD,
        Navigation
    ),
    cmd!(
        Reload,
        "reload",
        "Reload page",
        ["refresh"],
        Some("Ctrl+R"),
        crate::icons::RELOAD,
        Navigation
    ),
    cmd!(
        HardReload,
        "hard-reload",
        "Hard reload (bypass cache)",
        ["refresh", "cache", "force"],
        Some("Ctrl+Shift+R"),
        ph::ARROWS_CLOCKWISE,
        Navigation
    ),
    cmd!(
        Home,
        "home",
        "Go to home page",
        ["start", "new tab page"],
        Some("Alt+Home"),
        crate::icons::HOME,
        Navigation
    ),
    cmd!(
        FocusAddress,
        "focus-address",
        "Focus address bar",
        ["url", "location", "omnibox", "search"],
        Some("Ctrl+L"),
        ph::MAGNIFYING_GLASS,
        Navigation
    ),
    cmd!(
        CopyUrl,
        "copy-url",
        "Copy page address",
        ["link", "url", "clipboard"],
        None,
        ph::LINK,
        Navigation
    ),
    cmd!(
        Find,
        "find",
        "Find in page",
        ["search", "text"],
        Some("Ctrl+F"),
        ph::TEXT_AA,
        Page
    ),
    cmd!(
        ZoomIn,
        "zoom-in",
        "Zoom in",
        ["bigger", "magnify", "larger"],
        Some("Ctrl++"),
        ph::MAGNIFYING_GLASS_PLUS,
        Page
    ),
    cmd!(
        ZoomOut,
        "zoom-out",
        "Zoom out",
        ["smaller", "shrink"],
        Some("Ctrl+-"),
        ph::MAGNIFYING_GLASS_MINUS,
        Page
    ),
    cmd!(
        ZoomReset,
        "zoom-reset",
        "Reset zoom",
        ["100%", "actual size"],
        Some("Ctrl+0"),
        ph::MAGNIFYING_GLASS,
        Page
    ),
    cmd!(
        Fullscreen,
        "fullscreen",
        "Toggle full screen",
        ["maximize", "kiosk"],
        Some("F11"),
        ph::CORNERS_OUT,
        Page
    ),
    cmd!(
        Print,
        "print",
        "Print page…",
        ["pdf", "paper", "printer"],
        Some("Ctrl+Shift+P"),
        ph::PRINTER,
        Page
    ),
    cmd!(
        SavePage,
        "save-page",
        "Save page as…",
        ["download", "html", "pdf"],
        None,
        ph::FLOPPY_DISK,
        Page
    ),
    cmd!(
        ViewSource,
        "view-source",
        "View page source",
        ["html", "code"],
        Some("Ctrl+U"),
        ph::CODE,
        Page
    ),
    cmd!(
        Screenshot,
        "screenshot",
        "Screenshot of the visible page",
        ["capture", "image", "png", "snap"],
        Some("Ctrl+Shift+S"),
        ph::CAMERA,
        Capture
    ),
    cmd!(
        ScreenshotFullPage,
        "screenshot-full",
        "Screenshot of the full page",
        ["capture", "long", "scrolling", "png"],
        None,
        ph::FRAME_CORNERS,
        Capture
    ),
    cmd!(
        StartRecording,
        "recording-start",
        "Start screen recording",
        ["video", "record", "capture", "mp4", "gif"],
        None,
        ph::RECORD,
        Capture
    ),
    cmd!(
        StopRecording,
        "recording-stop",
        "Stop screen recording",
        ["video", "record", "finish"],
        None,
        ph::STOP_CIRCLE,
        Capture
    ),
    cmd!(
        DevTools,
        "devtools",
        "Developer tools",
        ["inspector", "elements", "debug", "network"],
        Some("F12"),
        ph::BUG,
        Developer
    ),
    cmd!(
        JsConsole,
        "js-console",
        "JavaScript console",
        ["log", "errors", "eval", "repl"],
        Some("Ctrl+Shift+J"),
        ph::TERMINAL_WINDOW,
        Developer
    ),
    cmd!(
        AutomationToggle,
        "automation-toggle",
        "Toggle automation (MCP) server",
        ["mcp", "api", "control", "agent", "socket"],
        None,
        ph::ROBOT,
        Developer
    ),
    cmd!(
        Bookmark,
        "bookmark",
        "Bookmark this page",
        ["star", "favorite", "save"],
        Some("Ctrl+D"),
        crate::icons::STAR,
        Library
    ),
    cmd!(
        BookmarksBar,
        "bookmarks-bar",
        "Show or hide the bookmarks bar",
        ["toolbar", "favorites"],
        Some("Ctrl+Shift+B"),
        ph::BOOKMARKS_SIMPLE,
        Library
    ),
    cmd!(
        BookmarksPage,
        "bookmarks-page",
        "Bookmarks manager",
        ["favorites", "library"],
        Some("Ctrl+Shift+O"),
        ph::BOOKMARKS,
        Library
    ),
    cmd!(
        History,
        "history",
        "History",
        ["visited", "recent", "past"],
        Some("Ctrl+H"),
        ph::CLOCK_COUNTER_CLOCKWISE,
        Library
    ),
    cmd!(
        ClearBrowsingData,
        "clear-data",
        "Clear browsing data…",
        ["cookies", "cache", "delete history", "privacy"],
        Some("Ctrl+Shift+Delete"),
        ph::BROOM,
        Privacy
    ),
    cmd!(
        Containers,
        "containers",
        "Containers",
        ["identities", "profiles", "isolation", "manage"],
        None,
        ph::SQUARES_FOUR,
        Privacy
    ),
    cmd!(
        Authenticator,
        "authenticator",
        "Authenticator (2FA codes)",
        ["totp", "otp", "two factor", "2fa", "code"],
        None,
        crate::icons::AUTHENTICATOR,
        Privacy
    ),
    cmd!(
        Passwords,
        "passwords",
        "Password manager",
        ["logins", "vault", "credentials", "keepass"],
        None,
        crate::icons::KEY,
        Privacy
    ),
    cmd!(
        Theme,
        "theme",
        "Change theme…",
        ["appearance", "colors", "colours", "dark", "light", "skin", "late night", "slate", "olive", "frost", "rose", "forest", "sand"],
        None,
        crate::icons::THEME,
        Appearance
    ),
    cmd!(
        DarkModeToggle,
        "dark-toggle",
        "Toggle dark mode for pages",
        ["night", "theme", "light", "invert"],
        None,
        crate::icons::MOON,
        Appearance
    ),
    cmd!(
        DarkModeSiteToggle,
        "dark-site-toggle",
        "Toggle dark mode for this site",
        ["night", "theme", "host"],
        None,
        ph::MOON_STARS,
        Appearance
    ),
    cmd!(
        DarkModeSettings,
        "dark-settings",
        "Dark mode settings",
        ["night", "theme", "preferences"],
        None,
        ph::PALETTE,
        Appearance
    ),
    cmd!(
        Plugins,
        "plugins",
        "Plugins",
        ["extensions", "scripts", "userscripts", "addons"],
        None,
        ph::PUZZLE_PIECE,
        Appearance
    ),
    cmd!(
        ReloadPlugins,
        "plugins-reload",
        "Reload plugins",
        ["extensions", "refresh"],
        None,
        ph::ARROWS_CLOCKWISE,
        Appearance
    ),
    cmd!(
        Settings,
        "settings",
        "Settings",
        ["preferences", "options", "configuration"],
        Some("Ctrl+,"),
        crate::icons::GEAR,
        Browser
    ),
    cmd!(
        Quit,
        "quit",
        "Quit BrowseCraft",
        ["exit", "close browser"],
        Some("Ctrl+Q"),
        ph::SIGN_OUT,
        Browser
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_and_ids_are_unique() {
        let keys: HashSet<_> = catalog().iter().map(|c| c.key).collect();
        let ids: HashSet<_> = catalog().iter().map(|c| c.id).collect();
        assert_eq!(keys.len(), catalog().len());
        assert_eq!(ids.len(), catalog().len());
    }

    #[test]
    fn key_round_trips() {
        for c in catalog() {
            assert_eq!(from_key(c.key), Some(c.id));
            assert_eq!(get(c.id).map(|x| x.key), Some(c.key));
        }
        assert_eq!(from_key("nope"), None);
    }

    #[test]
    fn page_commands_are_flagged() {
        assert!(needs_page(CommandId::Screenshot));
        assert!(!needs_page(CommandId::Settings));
        assert!(get(CommandId::Print).is_some_and(|c| c.needs_page()));
    }

    #[test]
    fn shortcut_uses_platform_modifier() {
        let shown = display_shortcut("Ctrl+T");
        if cfg!(target_os = "macos") {
            assert_eq!(shown, "Cmd+T");
        } else {
            assert_eq!(shown, "Ctrl+T");
        }
    }
}
