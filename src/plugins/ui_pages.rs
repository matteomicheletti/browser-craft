//! Plugin pages (API 1.1): `bc-plugin://<plugin-id>/<path>` served from the
//! plugin's own folder through a custom URI scheme.
//!
//! Only files of an enabled plugin that declares at least one page are
//! served, and only from inside its folder (`manifest::safe_join`: no `..`,
//! no absolute paths, no symlinks leading out). Every response carries a
//! strict Content-Security-Policy: the page's own files, inline styles, data
//! and blob images, and the https origins the manifest lists in
//! `remote_sources`, nothing else. A few dynamic resources live under
//! `/__host/`: the theme's CSS variables, the screenshot being edited, and
//! clipboard history images (with `clipboard.history` only).

use super::permissions::Permission;
use super::registry::Registry;
use std::borrow::Cow;

pub const SCHEME: &str = "bc-plugin";
/// Largest file served from a plugin folder.
const MAX_FILE: u64 = 8 * 1024 * 1024;

/// What the protocol handler answers.
#[derive(Debug, PartialEq)]
pub struct Served {
    pub status: u16,
    pub mime: &'static str,
    pub body: Cow<'static, [u8]>,
    pub csp: Option<String>,
}

impl Served {
    fn error(status: u16, msg: &'static str) -> Self {
        Served { status, mime: "text/plain; charset=utf-8", body: Cow::Borrowed(msg.as_bytes()), csp: None }
    }
}

/// `bc-plugin://<id>/<path>?<query>`.
pub fn page_url(plugin: &str, path: &str, query: &str) -> String {
    let q = if query.is_empty() { String::new() } else { format!("?{query}") };
    format!("{SCHEME}://{plugin}/{path}{q}")
}

/// A plugin page address taken apart.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRef {
    pub plugin: String,
    /// Decoded path without the leading `/`.
    pub path: String,
    pub query: String,
}

impl PageRef {
    /// The value of `key` in the query (`a=1&b=2`), not decoded (the browser
    /// only puts plain tokens there).
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
    }
}

/// Parses a `bc-plugin:` address; `None` for anything else or a bad id.
pub fn parse(url: &str) -> Option<PageRef> {
    let rest = url.strip_prefix(SCHEME)?.strip_prefix("://")?;
    let (host, rest) = rest.split_once('/').unwrap_or((rest, ""));
    let plugin = host.to_ascii_lowercase();
    if !super::manifest::valid_id(&plugin) {
        return None;
    }
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Some(PageRef { plugin, path: percent_decode(path)?, query: query.to_string() })
}

/// `%XX` decoding; `None` for broken escapes, NUL or invalid UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    (!s.contains('\0')).then_some(s)
}

fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" | "md" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// The Content-Security-Policy of a plugin's pages.
pub fn csp(remote: &[String]) -> String {
    let extra = remote.join(" ");
    let src = |base: &str| if extra.is_empty() { base.to_string() } else { format!("{base} {extra}") };
    format!(
        "default-src 'none'; script-src {}; style-src {}; img-src {}; font-src {}; connect-src {}; media-src 'self' blob: data:; \
         frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        src("'self'"),
        src("'self' 'unsafe-inline'"),
        src("'self' data: blob:"),
        src("'self' data:"),
        src("'self'"),
    )
}

/// Answers a request for `uri`. `referrer` is the Referer or Origin header
/// when the request has one: a plugin's files are only for its own pages,
/// so a web page cannot probe which plugins are installed by loading them.
pub fn serve(reg: &Registry, uri: &str, referrer: Option<&str>) -> Served {
    let Some(page) = parse(uri) else {
        return Served::error(400, "bad address");
    };
    if let Some(r) = referrer.filter(|r| !r.is_empty() && *r != "null")
        && parse(r).is_none_or(|from| from.plugin != page.plugin)
    {
        return Served::error(403, "plugin files are only served to the plugin's own pages");
    }
    let Some(plugin) = reg.enabled().find(|p| p.manifest.id == page.plugin) else {
        return Served::error(404, "no such plugin, or it is turned off");
    };
    let m = &plugin.manifest;
    if m.pages.is_empty() {
        return Served::error(404, "this plugin has no pages");
    }
    let csp = Some(csp(&m.remote_sources));
    if let Some(dynamic) = page.path.strip_prefix("__host/") {
        return match dynamic {
            "theme.css" => Served {
                status: 200,
                mime: "text/css; charset=utf-8",
                body: Cow::Owned(super::host::theme_css(crate::internal::current()).into_bytes()),
                csp,
            },
            d => match d.strip_prefix("blob/").and_then(|t| super::host::blob_get(&m.id, t)) {
                Some((mime, bytes)) => Served { status: 200, mime, body: Cow::Owned(bytes.to_vec()), csp },
                None => match d.strip_prefix("clip/").and_then(|f| f.strip_suffix(".png")).and_then(|id| id.parse::<u64>().ok()) {
                    Some(id) if m.has(Permission::ClipboardHistory) && page.param("k") == Some(super::host::clip_key()) => match super::clipboard_history::image_bytes(id) {
                        Some(bytes) => Served { status: 200, mime: "image/png", body: Cow::Owned(bytes), csp },
                        None => Served::error(404, "no such image"),
                    },
                    _ => Served::error(404, "not found"),
                },
            },
        };
    }
    let path = if page.path.is_empty() { m.pages[0].path.clone() } else { page.path.clone() };
    let file = match super::manifest::safe_join(&plugin.dir, &path) {
        Ok(f) => f,
        Err(_) => return Served::error(404, "not found"),
    };
    match crate::storage::read_limited(&file, MAX_FILE) {
        Ok(bytes) => Served { status: 200, mime: mime_for(&path), body: Cow::Owned(bytes), csp },
        Err(_) => Served::error(404, "not found"),
    }
}

/// The handler wry calls for the scheme (on the GTK main thread).
pub fn handle(request: &wry::http::Request<Vec<u8>>) -> wry::http::Response<Cow<'static, [u8]>> {
    let referrer = request
        .headers()
        .get("origin")
        .or_else(|| request.headers().get("referer"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let served = {
        let reg = super::shared();
        let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        serve(&reg, &request.uri().to_string(), referrer.as_deref())
    };
    let mut b = wry::http::Response::builder()
        .status(served.status)
        .header("Content-Type", served.mime)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store");
    if let Some(csp) = &served.csp {
        b = b.header("Content-Security-Policy", csp.as_str());
    }
    b.body(served.body).unwrap_or_else(|_| wry::http::Response::new(Cow::Borrowed(&b"error"[..])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_parsed_strictly() {
        let p = parse("bc-plugin://local.me.clip/sub/a%20b.html?shot=ab12#x").unwrap();
        assert_eq!((p.plugin.as_str(), p.path.as_str(), p.param("shot")), ("local.me.clip", "sub/a b.html", Some("ab12")));
        assert_eq!(parse("bc-plugin://local.me.clip").unwrap().path, "");
        for bad in ["https://local.me.clip/a", "bc-plugin://NotAnId/a", "bc-plugin://x/a", "bc-plugin://a.b/%zz", "bc-plugin://a.b/%00", "bc-plugin:/a.b/x"] {
            assert!(parse(bad).is_none(), "{bad}");
        }
        assert_eq!(page_url("a.b", "x.html", "q=1"), "bc-plugin://a.b/x.html?q=1");
    }

    fn reg_with_page() -> (tempfile::TempDir, Registry) {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        std::fs::create_dir_all(dev.join("ui")).unwrap();
        std::fs::write(
            dev.join("plugin.toml"),
            "id = \"com.t.ui\"\nname = \"UI\"\nversion = \"1.0.0\"\napi_version = \"1.1\"\nremote_sources = [\"https://cdn.example.com\"]\n\
             [[pages]]\nid = \"main\"\npath = \"ui/index.html\"\n",
        )
        .unwrap();
        std::fs::write(dev.join("ui/index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(dev.join("ui/app.js"), "1").unwrap();
        std::fs::write(d.path().join("secret.txt"), "s").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.path().join("secret.txt"), dev.join("ui/link.txt")).unwrap();
        let mut r = Registry::open(d.path().join("cfg"));
        r.add_dev_folder(&dev).unwrap();
        (d, r)
    }

    fn tiny_png() -> Vec<u8> {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([9, 9, 9, 255])))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    #[test]
    fn serves_only_the_plugins_own_files() {
        let (_d, mut r) = reg_with_page();
        let ok = serve(&r, "bc-plugin://com.t.ui/ui/index.html", None);
        assert_eq!((ok.status, ok.mime), (200, "text/html; charset=utf-8"));
        assert_eq!(&*ok.body, b"<h1>hi</h1>");
        let csp = ok.csp.unwrap();
        assert!(csp.contains("script-src 'self' https://cdn.example.com;") && csp.contains("frame-ancestors 'none'"));
        assert_eq!(serve(&r, "bc-plugin://com.t.ui/", None).status, 200, "the first page is the index");
        assert_eq!(serve(&r, "bc-plugin://com.t.ui/ui/app.js", None).mime, "text/javascript; charset=utf-8");
        for bad in [
            "bc-plugin://com.t.ui/../secret.txt",
            "bc-plugin://com.t.ui/ui/%2e%2e/%2e%2e/secret.txt",
            "bc-plugin://com.t.ui//etc/passwd",
            "bc-plugin://com.t.ui/ui/link.txt",
            "bc-plugin://com.t.ui/plugin.toml.missing",
            "bc-plugin://com.t.other/ui/index.html",
        ] {
            assert_ne!(serve(&r, bad, None).status, 200, "{bad}");
        }
        // Only the plugin's own pages may load its files.
        assert_eq!(serve(&r, "bc-plugin://com.t.ui/ui/app.js", Some("https://evil.test/")).status, 403);
        assert_eq!(serve(&r, "bc-plugin://com.t.ui/ui/app.js", Some("bc-plugin://com.t.ui/ui/index.html")).status, 200);
        assert_eq!(serve(&r, "bc-plugin://com.t.ui/__host/theme.css", None).status, 200);
        let key = crate::plugins::host::clip_key();
        assert_eq!(serve(&r, &format!("bc-plugin://com.t.ui/__host/clip/1.png?k={key}"), None).status, 404, "needs clipboard.history");
        // Clipboard images: only with the permission and the page's key.
        let toml = r.get("com.t.ui").and_then(|e| e.dir.clone()).unwrap().join("plugin.toml");
        let text = std::fs::read_to_string(&toml).unwrap().replace("remote_sources", "permissions = [\"clipboard.history\"]\nremote_sources");
        std::fs::write(&toml, text).unwrap();
        r.reload();
        let id = crate::plugins::clipboard_history::with(|s| {
            let c = crate::plugins::clipboard_history::Capture {
                image: crate::plugins::clipboard_watch::inspect_png(tiny_png()),
                ..Default::default()
            };
            s.add(c, &crate::plugins::clipboard_history::Limits::default(), 1).unwrap()
        })
        .unwrap();
        assert_eq!(serve(&r, &format!("bc-plugin://com.t.ui/__host/clip/{id}.png?k={key}"), None).status, 200);
        assert_eq!(serve(&r, &format!("bc-plugin://com.t.ui/__host/clip/{id}.png"), None).status, 404, "no key");
        assert_eq!(serve(&r, &format!("bc-plugin://com.t.ui/__host/clip/{id}.png?k=guess"), None).status, 404);
        // Turned off: nothing is served.
        r.set_enabled("com.t.ui", false).unwrap();
        assert_eq!(serve(&r, "bc-plugin://com.t.ui/ui/index.html", None).status, 404);
    }
}
