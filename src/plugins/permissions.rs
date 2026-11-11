//! Permissions a plugin can ask for and the API methods they gate.
//!
//! A manifest lists the permissions it needs; installing it after seeing the
//! list is the grant (there is no per-permission prompt yet). Every bridge
//! call is checked against the plugin's own list, so a plugin without
//! `notifications` cannot raise one however it phrases the message.

use super::pattern::MatchPattern;

/// Version of the plugin API this build implements (`api_version` in the
/// manifest). Same major = compatible; a plugin may not require a newer minor.
pub const API_MAJOR: u32 = 1;
pub const API_MINOR: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    TabsRead,
    TabsOpen,
    PageRead,
    PageModify,
    Storage,
    Notifications,
    ClipboardWrite,
    /// API 1.1: read and manage the clipboard history (plugin pages only).
    ClipboardHistory,
    /// API 1.1: receive the browser's screenshots and copy or save images.
    ScreenshotEdit,
    /// API 1.1: bind keys the browser or text editing already uses.
    ShortcutsOverride,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Low,
    Medium,
    High,
}

pub struct PermissionInfo {
    pub perm: Permission,
    pub name: &'static str,
    pub risk: Risk,
    pub description: &'static str,
}

pub const ALL: &[PermissionInfo] = &[
    PermissionInfo {
        perm: Permission::PageRead,
        name: "page.read",
        risk: Risk::High,
        description: "Run JavaScript in pages the plugin matches and read their content, including what you type there.",
    },
    PermissionInfo {
        perm: Permission::PageModify,
        name: "page.modify",
        risk: Risk::High,
        description: "Change pages the plugin matches: inject styles and scripts, edit the DOM.",
    },
    PermissionInfo {
        perm: Permission::TabsRead,
        name: "tabs.read",
        risk: Risk::Medium,
        description: "See the titles and addresses of your open tabs.",
    },
    PermissionInfo {
        perm: Permission::TabsOpen,
        name: "tabs.open",
        risk: Risk::Medium,
        description: "Open new tabs.",
    },
    PermissionInfo {
        perm: Permission::Storage,
        name: "storage",
        risk: Risk::Low,
        description: "Keep a small amount of private data (up to 256 KB) for itself.",
    },
    PermissionInfo {
        perm: Permission::Notifications,
        name: "notifications",
        risk: Risk::Low,
        description: "Show notifications in the browser.",
    },
    PermissionInfo {
        perm: Permission::ClipboardWrite,
        name: "clipboard.write",
        risk: Risk::Medium,
        description: "Put text on the clipboard (user gesture not enforced yet).",
    },
    PermissionInfo {
        perm: Permission::ClipboardHistory,
        name: "clipboard.history",
        risk: Risk::High,
        description: "Record everything copied, in the browser and in other apps (text, images, file lists), and paste it into pages. Copies from the password manager and the authenticator are never recorded.",
    },
    PermissionInfo {
        perm: Permission::ScreenshotEdit,
        name: "screenshot.edit",
        risk: Risk::Medium,
        description: "Receive the screenshots you take, put images on the clipboard and save image files to Pictures.",
    },
    PermissionInfo {
        perm: Permission::ShortcutsOverride,
        name: "shortcuts.override",
        risk: Risk::Medium,
        description: "Take over keys the browser or text editing already uses (for example Ctrl+V).",
    },
];

impl Permission {
    pub fn info(self) -> &'static PermissionInfo {
        // Every variant is listed in ALL (checked by a test).
        ALL.iter().find(|i| i.perm == self).unwrap_or(&ALL[0])
    }

    pub fn name(self) -> &'static str {
        self.info().name
    }

    pub fn from_name(name: &str) -> Option<Self> {
        ALL.iter().find(|i| i.name == name).map(|i| i.perm)
    }
}

/// What a manifest `permissions` entry asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Requested {
    Api(Permission),
    /// A host pattern. Reserved: no API takes a host permission yet (content
    /// scripts are limited by their own `matches`); shown so manifests can
    /// already declare the future network access honestly.
    Host(MatchPattern),
}

impl Requested {
    pub fn parse(s: &str) -> Result<Self, String> {
        if let Some(p) = Permission::from_name(s) {
            return Ok(Self::Api(p));
        }
        if s == "<all_urls>" || s.contains("://") {
            return MatchPattern::parse(s).map(Self::Host);
        }
        Err(format!("unknown permission \"{s}\""))
    }

    pub fn label(&self) -> String {
        match self {
            Self::Api(p) => p.name().into(),
            Self::Host(h) => format!("host: {}", h.as_str()),
        }
    }
}

/// One method of the page-side `bc` object.
pub struct ApiMethod {
    /// Wire name (also the bridge `m` field).
    pub wire: &'static str,
    /// JavaScript signature shown in the manager.
    pub js: &'static str,
    pub needs: Option<Permission>,
    pub description: &'static str,
}

pub const API: &[ApiMethod] = &[
    ApiMethod { wire: "log", js: "bc.log(...args)", needs: None, description: "Write a line to the browser's plugin log (rate limited)." },
    ApiMethod { wire: "message", js: "bc.sendMessage(obj) -> Promise", needs: None, description: "Send a JSON object to the plugin's browser-side handler (its WASM module, if any)." },
    ApiMethod { wire: "storage.get", js: "bc.storage.get(key) -> Promise", needs: Some(Permission::Storage), description: "Read a stored JSON value." },
    ApiMethod { wire: "storage.set", js: "bc.storage.set(key, value) -> Promise", needs: Some(Permission::Storage), description: "Store a JSON value (limits apply)." },
    ApiMethod { wire: "storage.remove", js: "bc.storage.remove(key) -> Promise", needs: Some(Permission::Storage), description: "Delete a stored value." },
    ApiMethod { wire: "notify", js: "bc.notify(title, body) -> Promise", needs: Some(Permission::Notifications), description: "Show a notification." },
    ApiMethod { wire: "clipboard.write", js: "bc.clipboard.writeText(text) -> Promise", needs: Some(Permission::ClipboardWrite), description: "Copy text to the clipboard." },
    ApiMethod { wire: "tabs.query", js: "bc.tabs.query() -> Promise<[{id,title,url,active}]>", needs: Some(Permission::TabsRead), description: "List open tabs." },
    ApiMethod { wire: "tabs.open", js: "bc.tabs.open(url) -> Promise", needs: Some(Permission::TabsOpen), description: "Open an http(s) address in a new tab." },
];

pub fn method(wire: &str) -> Option<&'static ApiMethod> {
    API.iter().find(|m| m.wire == wire)
}

/// Checks the declared permissions against what `wire` needs.
pub fn check(granted: &[Requested], wire: &str) -> Result<(), String> {
    let m = method(wire).ok_or_else(|| format!("unknown API method \"{wire}\""))?;
    match m.needs {
        None => Ok(()),
        Some(p) if granted.contains(&Requested::Api(p)) => Ok(()),
        Some(p) => Err(format!("permission \"{}\" is required for {}", p.name(), m.js)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_permission_is_documented() {
        for p in [
            Permission::TabsRead, Permission::TabsOpen, Permission::PageRead, Permission::PageModify,
            Permission::Storage, Permission::Notifications, Permission::ClipboardWrite,
            Permission::ClipboardHistory, Permission::ScreenshotEdit, Permission::ShortcutsOverride,
        ] {
            assert_eq!(p.info().perm, p);
            assert_eq!(Permission::from_name(p.name()), Some(p));
        }
        assert_eq!(ALL.len(), 10);
    }

    #[test]
    fn requested_parses_apis_and_hosts() {
        assert_eq!(Requested::parse("storage"), Ok(Requested::Api(Permission::Storage)));
        assert!(matches!(Requested::parse("*://*.a.test/*"), Ok(Requested::Host(_))));
        assert!(matches!(Requested::parse("<all_urls>"), Ok(Requested::Host(_))));
        assert!(Requested::parse("root").is_err());
        assert!(Requested::parse("https://bad").is_err());
    }

    #[test]
    fn check_enforces_declared_permissions() {
        let g = [Requested::Api(Permission::Storage)];
        assert!(check(&g, "storage.get").is_ok());
        assert!(check(&g, "log").is_ok());
        assert!(check(&g, "notify").unwrap_err().contains("notifications"));
        assert!(check(&g, "tabs.query").is_err());
        assert!(check(&g, "nope").is_err());
    }
}
