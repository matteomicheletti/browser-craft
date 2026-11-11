//! "Open page in another browser": hands a web page's address to the
//! system's default browser (`xdg-open` / `gio open` on Linux, `open` on
//! macOS, the URL handler on Windows). Internal pages never leave.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The address to hand over, if `url` is a web page (http or https).
pub fn shareable(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()).then(|| parsed.to_string())
}

/// Programs tried in order, each with its arguments before the address.
fn openers() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(target_os = "macos") {
        &[("open", &[])]
    } else if cfg!(windows) {
        &[("rundll32", &["url.dll,FileProtocolHandler"])]
    } else {
        &[("xdg-open", &[]), ("gio", &["open"])]
    }
}

/// Opens `url` in the default browser. Only web pages: an internal or
/// file address is refused.
pub fn open_in_default_browser(url: &str) -> Result<(), String> {
    let url = shareable(url).ok_or("Only web pages can be opened in another browser")?;
    let mut last = String::new();
    for (program, args) in openers() {
        match Command::new(program)
            .args(*args)
            .arg(&url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Reaped in the background: no zombie, no waiting here.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) => last = format!("{program}: {e}"),
        }
    }
    Err(format!("Could not start the default browser ({last})"))
}

/// The default browser's name for buttons ("Firefox"), or "your default
/// browser" when it cannot be found. Looked up once.
pub fn default_browser_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Ok(out) = Command::new("xdg-settings").args(["get", "default-web-browser"]).stderr(Stdio::null()).output()
            && let Some(name) = desktop_name(&String::from_utf8_lossy(&out.stdout))
        {
            return name;
        }
        "your default browser".to_string()
    })
}

/// "firefox.desktop" -> "Firefox", "org.gnome.Epiphany.desktop" -> "Epiphany",
/// "google-chrome.desktop" -> "Google Chrome".
fn desktop_name(id: &str) -> Option<String> {
    let id = id.trim().strip_suffix(".desktop")?;
    let last = id.rsplit('.').next()?;
    let words: Vec<String> = last
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        })
        .collect();
    let name = words.join(" ");
    (!name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_alphanumeric() || c == ' ')).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pages_leave() {
        assert_eq!(shareable("https://teams.microsoft.com/v2/").as_deref(), Some("https://teams.microsoft.com/v2/"));
        assert!(shareable("http://a.test").is_some());
        for url in ["browsecraft://flags", "about:home", "file:///etc/passwd", "view-source:https://a.test", "javascript:alert(1)", "", "-h"] {
            assert_eq!(shareable(url), None, "{url}");
        }
        assert!(open_in_default_browser("browsecraft://settings").is_err());
    }

    #[test]
    fn desktop_ids_become_names() {
        assert_eq!(desktop_name("firefox.desktop\n").as_deref(), Some("Firefox"));
        assert_eq!(desktop_name("org.gnome.Epiphany.desktop").as_deref(), Some("Epiphany"));
        assert_eq!(desktop_name("google-chrome.desktop").as_deref(), Some("Google Chrome"));
        assert_eq!(desktop_name(""), None);
        assert_eq!(desktop_name("firefox"), None);
    }
}
