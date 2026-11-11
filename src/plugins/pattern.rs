//! WebExtension-style match patterns (`<all_urls>`, `*://*.example.com/a/*`).
//!
//! Supported: schemes `*` (http and https), `http`, `https`, `file`, `ftp`;
//! hosts `*`, `*.domain` and exact names; paths with `*` wildcards, matched
//! against path plus query (never the fragment). Ports are ignored when
//! matching and not allowed in patterns, as in Firefox. `<all_urls>` covers
//! http, https, file and ftp only: never `bc:`, `about:`, `data:` or similar.

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum SchemeMatch {
    /// `*`: http or https.
    WebAny,
    /// `<all_urls>`: http, https, file, ftp.
    AllUrls,
    Exact(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum HostMatch {
    Any,
    /// `*.example.com`: the domain itself and every subdomain.
    Suffix(String),
    Exact(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchPattern {
    raw: String,
    pub scheme: SchemeMatch,
    pub host: HostMatch,
    pub path: String,
}

const SCHEMES: [&str; 4] = ["http", "https", "file", "ftp"];

impl MatchPattern {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("invalid match pattern \"{raw}\": {why}");
        if raw.len() > 512 || raw.contains(char::is_control) {
            return Err(bad("too long or contains control characters"));
        }
        if raw == "<all_urls>" {
            return Ok(Self { raw: raw.into(), scheme: SchemeMatch::AllUrls, host: HostMatch::Any, path: "*".into() });
        }
        let (scheme, rest) = raw.split_once("://").ok_or_else(|| bad("expected scheme://host/path"))?;
        let scheme = match scheme {
            "*" => SchemeMatch::WebAny,
            s if SCHEMES.contains(&s) => SchemeMatch::Exact(s.into()),
            _ => return Err(bad("scheme must be *, http, https, file or ftp")),
        };
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => return Err(bad("missing path (use /* for everything)")),
        };
        let host = if host == "*" {
            HostMatch::Any
        } else if let Some(domain) = host.strip_prefix("*.") {
            if domain.is_empty() || domain.contains('*') {
                return Err(bad("wildcard only allowed as a leading *."));
            }
            HostMatch::Suffix(check_host(domain).ok_or_else(|| bad("bad host"))?)
        } else if host.is_empty() {
            if scheme != SchemeMatch::Exact("file".into()) {
                return Err(bad("empty host is only valid for file://"));
            }
            HostMatch::Exact(String::new())
        } else {
            if host.contains('*') {
                return Err(bad("wildcard only allowed as * or a leading *."));
            }
            HostMatch::Exact(check_host(host).ok_or_else(|| bad("bad host (ports are not allowed)"))?)
        };
        Ok(Self { raw: raw.into(), scheme, host, path: path.into() })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn matches_url(&self, url: &str) -> bool {
        let Ok(u) = url::Url::parse(url) else { return false };
        let target = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_string(),
        };
        self.matches_parts(u.scheme(), u.host_str().unwrap_or(""), &target)
    }

    /// `host` lowercase without port, `target` = path plus query.
    pub fn matches_parts(&self, scheme: &str, host: &str, target: &str) -> bool {
        let scheme_ok = match &self.scheme {
            SchemeMatch::WebAny => scheme == "http" || scheme == "https",
            SchemeMatch::AllUrls => SCHEMES.contains(&scheme),
            SchemeMatch::Exact(s) => s == scheme,
        };
        if !scheme_ok {
            return false;
        }
        let host_ok = match &self.host {
            HostMatch::Any => true,
            HostMatch::Exact(h) => h == host,
            HostMatch::Suffix(d) => host == d || host.strip_suffix(d.as_str()).is_some_and(|p| p.ends_with('.')),
        };
        host_ok && glob(&self.path, target)
    }
}

fn check_host(h: &str) -> Option<String> {
    let ok = !h.is_empty()
        && h.len() <= 253
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    ok.then(|| h.to_ascii_lowercase())
}

/// `*` matches any run of characters (including none), everything else
/// literally. Iterative with backtracking, so no stack growth on long input.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// Any of `include` matches and none of `exclude` does.
pub fn matches_any(include: &[MatchPattern], exclude: &[MatchPattern], url: &str) -> bool {
    include.iter().any(|p| p.matches_url(url)) && !exclude.iter().any(|p| p.matches_url(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, url: &str) -> bool {
        MatchPattern::parse(p).unwrap().matches_url(url)
    }

    #[test]
    fn all_urls_is_web_and_files_only() {
        assert!(m("<all_urls>", "https://a.test/x"));
        assert!(m("<all_urls>", "file:///tmp/a.html"));
        assert!(!m("<all_urls>", "about:blank"));
        assert!(!m("<all_urls>", "data:text/html,hi"));
        assert!(!m("<all_urls>", "bc://settings"));
    }

    #[test]
    fn hosts_and_subdomains() {
        assert!(m("*://*.example.com/*", "https://example.com/"));
        assert!(m("*://*.example.com/*", "http://a.b.example.com/x?y=1"));
        assert!(!m("*://*.example.com/*", "https://badexample.com/"));
        assert!(!m("*://*.example.com/*", "https://example.com.evil.test/"));
        assert!(m("https://example.com/*", "https://example.com:8443/p"));
        assert!(!m("https://example.com/*", "http://example.com/"));
        assert!(m("*://example.com/*", "https://EXAMPLE.com/"));
        assert!(!m("*://example.com/*", "ftp://example.com/"));
        assert!(m("*://*/*", "http://x.test/"));
    }

    #[test]
    fn paths_and_queries() {
        assert!(m("https://a.test/docs/*", "https://a.test/docs/x/y"));
        assert!(!m("https://a.test/docs/*", "https://a.test/doc"));
        assert!(m("https://a.test/", "https://a.test/"));
        assert!(!m("https://a.test/", "https://a.test/x"));
        assert!(m("https://a.test/s?q=*", "https://a.test/s?q=rust"));
        assert!(!m("https://a.test/s", "https://a.test/s?q=rust"));
        assert!(m("https://a.test/*", "https://a.test/s#frag"));
        assert!(m("https://a.test/*/end", "https://a.test/a/b/end"));
        assert!(!m("https://a.test/*/end", "https://a.test/a/b/end2"));
    }

    #[test]
    fn file_scheme() {
        assert!(m("file:///tmp/*", "file:///tmp/a.html"));
        assert!(!m("file:///tmp/*", "file:///etc/passwd"));
    }

    #[test]
    fn invalid_patterns_are_rejected() {
        for bad in [
            "", "example.com", "https://example.com", "javascript://x/*", "https://exa*mple.com/*",
            "https://*example.com/*", "https://*./*", "https://example.com:8080/*", "://a/*",
            "<ALL_URLS>", "*://a b/*", "http:///x",
        ] {
            assert!(MatchPattern::parse(bad).is_err(), "{bad}");
        }
        assert!(MatchPattern::parse(&format!("https://a.test/{}", "x".repeat(600))).is_err());
    }

    #[test]
    fn exclude_wins_and_glob_is_linear_enough() {
        let inc = [MatchPattern::parse("https://a.test/*").unwrap()];
        let exc = [MatchPattern::parse("https://a.test/private/*").unwrap()];
        assert!(matches_any(&inc, &exc, "https://a.test/x"));
        assert!(!matches_any(&inc, &exc, "https://a.test/private/x"));
        let long = format!("https://a.test/{}", "ab".repeat(20_000));
        assert!(!MatchPattern::parse("https://a.test/*a*a*a*a*c").unwrap().matches_url(&long));
    }
}
