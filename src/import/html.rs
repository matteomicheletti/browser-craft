//! The Netscape bookmark file format (`bookmarks.html`), which every
//! browser can export and import:
//!
//! ```text
//! <!DOCTYPE NETSCAPE-Bookmark-file-1>
//! <DL><p>
//!     <DT><H3 ADD_DATE="1700000000" PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
//!     <DL><p>
//!         <DT><A HREF="https://example.com/" ADD_DATE="1700000000">Example</A>
//!     </DL><p>
//! </DL><p>
//! ```
//!
//! The parser is a small tag scanner, not an HTML engine: it reads `<H3>`
//! (a folder whose items are in the next `<DL>`), `<A HREF>` (a bookmark)
//! and `</DL>`, and ignores everything else, so a sloppy file still
//! imports. A top-level folder marked `PERSONAL_TOOLBAR_FOLDER` is the bar;
//! the rest goes to "Other bookmarks".

use crate::bookmarks::{ImportItem, ImportedBookmarks, MAX_DEPTH, Node, Tree};

/// The largest file read.
pub const MAX_FILE: u64 = 64 << 20;
/// Most bookmarks and folders read from one file.
const MAX_ITEMS: usize = 200_000;

/// A tag: its lower-case name (with a leading `/` when closing) and
/// attributes (lower-case names).
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Parses the inside of `<...>`.
fn parse_tag(inner: &str) -> Tag {
    let mut chars = inner.char_indices().peekable();
    let mut name = String::new();
    while let Some((_, c)) = chars.peek().copied() {
        if c.is_whitespace() || c == '>' {
            break;
        }
        name.push(c.to_ascii_lowercase());
        chars.next();
    }
    let mut attrs = Vec::new();
    loop {
        while chars.peek().is_some_and(|(_, c)| c.is_whitespace() || *c == '/') {
            chars.next();
        }
        let mut key = String::new();
        while let Some((_, c)) = chars.peek().copied() {
            if c.is_whitespace() || c == '=' {
                break;
            }
            key.push(c.to_ascii_lowercase());
            chars.next();
        }
        if key.is_empty() {
            break;
        }
        while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
            chars.next();
        }
        let mut value = String::new();
        if chars.peek().is_some_and(|(_, c)| *c == '=') {
            chars.next();
            while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
                chars.next();
            }
            match chars.peek().copied() {
                Some((_, q @ ('"' | '\''))) => {
                    chars.next();
                    for (_, c) in chars.by_ref() {
                        if c == q {
                            break;
                        }
                        value.push(c);
                    }
                }
                _ => {
                    while let Some((_, c)) = chars.peek().copied() {
                        if c.is_whitespace() {
                            break;
                        }
                        value.push(c);
                        chars.next();
                    }
                }
            }
        }
        attrs.push((key, decode_entities(&value)));
    }
    Tag { name, attrs }
}

/// Decodes the character references bookmark files use.
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[..rest.len().min(12)].find(';');
        let decoded = end.and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of an `<A>` or `<H3>`: up to the next tag (titles hold no
/// markup), skipping the closing `</name>` if that is the tag. A missing
/// closing tag ends the title at the next tag instead of swallowing it.
fn text_until<'a>(s: &'a str, name: &str) -> (&'a str, &'a str) {
    let end = s.find('<').unwrap_or(s.len());
    let (text, rest) = s.split_at(end);
    let b = rest.as_bytes();
    if b.len() >= 2 + name.len() && b[1] == b'/' && b[2..2 + name.len()].eq_ignore_ascii_case(name.as_bytes()) {
        let after = rest.find('>').map_or(rest.len(), |j| j + 1);
        return (text, &rest[after..]);
    }
    (text, rest)
}

/// Removes any tags inside a title and decodes its entities.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(out.trim())
}

/// Parses a bookmarks file.
pub fn parse(text: &str) -> Result<ImportedBookmarks, String> {
    if !text.get(..text.len().min(4096)).unwrap_or("").to_ascii_lowercase().contains("<dl") {
        return Err("This is not a bookmarks file (no <DL> list).".into());
    }
    // The open folders: (title, added, toolbar, items); the first is the top level.
    let mut stack: Vec<(String, i64, bool, Vec<ImportItem>)> = vec![(String::new(), 0, false, Vec::new())];
    // A folder heading waiting for its <DL>.
    let mut pending: Option<(String, i64, bool)> = None;
    // <DL>s opened beyond MAX_DEPTH: their items go into the deepest folder.
    let mut flattened = 0usize;
    let mut count = 0usize;
    let mut started = false;
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt + 1..];
        let Some(gt) = rest.find('>') else { break };
        let tag = parse_tag(&rest[..gt]);
        rest = &rest[gt + 1..];
        match tag.name.as_str() {
            "dl" => {
                if !started {
                    started = true;
                    pending = None;
                    continue;
                }
                match pending.take() {
                    Some((title, added, toolbar)) if stack.len() <= MAX_DEPTH => stack.push((title, added, toolbar, Vec::new())),
                    _ => flattened += 1,
                }
            }
            "/dl" => {
                if flattened > 0 {
                    flattened -= 1;
                } else if stack.len() > 1
                    && let Some((title, added, toolbar, items)) = stack.pop()
                {
                    let folder = ImportItem::Folder { title, added, items };
                    let top_level = stack.len() == 1;
                    if let Some(parent) = stack.last_mut() {
                        parent.3.push(folder);
                        if toolbar && top_level {
                            parent.2 = true; // remember which top-level folder was the bar
                            parent.0 = format!("{}", parent.3.len() - 1);
                        }
                    }
                }
            }
            "h3" => {
                let (inner, after) = text_until(rest, "h3");
                rest = after;
                count += 1;
                let toolbar = tag.attr("personal_toolbar_folder").is_some_and(|v| v.eq_ignore_ascii_case("true"));
                let added = tag.attr("add_date").and_then(|d| d.parse().ok()).unwrap_or(0);
                pending = Some((plain(inner), added, toolbar));
            }
            "a" => {
                let (inner, after) = text_until(rest, "a");
                rest = after;
                let Some(href) = tag.attr("href") else { continue };
                count += 1;
                let added = tag.attr("add_date").and_then(|d| d.parse().ok()).unwrap_or(0);
                if let Some(top) = stack.last_mut() {
                    top.3.push(ImportItem::Link { title: plain(inner), url: href.trim().to_string(), added });
                }
            }
            _ => {}
        }
        if count > MAX_ITEMS {
            return Err(format!("The file has more than {MAX_ITEMS} bookmarks and folders."));
        }
    }
    // Close folders a truncated file left open.
    while stack.len() > 1 {
        if let Some((title, added, _, items)) = stack.pop()
            && let Some(parent) = stack.last_mut()
        {
            parent.3.push(ImportItem::Folder { title, added, items });
        }
    }
    let Some((bar_index, _, has_bar, mut top)) = stack.pop() else {
        return Ok(ImportedBookmarks::default());
    };
    let mut out = ImportedBookmarks { bar_title: "Bookmarks bar".into(), ..Default::default() };
    if has_bar
        && let Ok(i) = bar_index.parse::<usize>()
        && i < top.len()
        && let ImportItem::Folder { title, items, .. } = top.remove(i)
    {
        out.bar_title = title;
        out.bar = items;
    }
    out.other = top;
    Ok(out)
}

/// Reads and parses a bookmarks file.
pub fn parse_file(path: &std::path::Path) -> Result<ImportedBookmarks, String> {
    let bytes = crate::storage::read_limited(path, MAX_FILE).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&String::from_utf8_lossy(&bytes))
}

fn escape(text: &str) -> String {
    crate::internal::escape(text)
}

fn write_items(out: &mut String, items: &[Node], indent: usize) {
    let pad = "    ".repeat(indent);
    for node in items {
        match &node.url {
            Some(url) => out.push_str(&format!(
                "{pad}<DT><A HREF=\"{}\" ADD_DATE=\"{}\">{}</A>\n",
                escape(url),
                node.added.max(0),
                escape(&node.title)
            )),
            None => {
                out.push_str(&format!("{pad}<DT><H3 ADD_DATE=\"{}\">{}</H3>\n{pad}<DL><p>\n", node.added.max(0), escape(&node.title)));
                write_items(out, &node.children, indent + 1);
                out.push_str(&format!("{pad}</DL><p>\n"));
            }
        }
    }
}

/// The bookmarks as a bookmarks file: the bar as a folder marked
/// `PERSONAL_TOOLBAR_FOLDER` (as Chrome and Firefox write it), then the
/// contents of "Other bookmarks".
pub fn export(tree: &Tree) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<!-- This is an automatically generated file.\n     It will be read and overwritten.\n     DO NOT EDIT! -->\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n<TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    out.push_str(&format!(
        "    <DT><H3 ADD_DATE=\"{}\" PERSONAL_TOOLBAR_FOLDER=\"true\">{}</H3>\n    <DL><p>\n",
        tree.bar.added.max(0),
        escape(&tree.bar.title)
    ));
    write_items(&mut out, &tree.bar.children, 2);
    out.push_str("    </DL><p>\n");
    write_items(&mut out, &tree.other.children, 1);
    out.push_str("</DL><p>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_EXPORT: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<!-- This is an automatically generated file. -->
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks</H1>
<DL><p>
    <DT><H3 ADD_DATE="1700000000" LAST_MODIFIED="0" PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
    <DL><p>
        <DT><A HREF="https://rust-lang.org/" ADD_DATE="1700000001" ICON="data:image/png;base64,AAAA">Rust &amp; friends</A>
        <DT><H3 ADD_DATE="1700000002">Work</H3>
        <DL><p>
            <DT><A HREF="https://mail.test/?a=1&amp;b=2">Mail &#8212; inbox</A>
        </DL><p>
    </DL><p>
    <DT><A HREF="https://other.test/">Other</A>
    <DT><H3>Folder</H3>
    <DL><p>
        <DT><A href='https://single.test/'>Single quotes</a>
    </DL><p>
</DL><p>
"#;

    #[test]
    fn parses_a_chrome_export() {
        let b = parse(CHROME_EXPORT).unwrap();
        assert_eq!(b.bar_title, "Bookmarks bar");
        assert_eq!(b.bar[0], ImportItem::Link { title: "Rust & friends".into(), url: "https://rust-lang.org/".into(), added: 1_700_000_001 });
        match &b.bar[1] {
            ImportItem::Folder { title, items, added } => {
                assert_eq!((title.as_str(), *added), ("Work", 1_700_000_002));
                assert_eq!(items[0], ImportItem::Link { title: "Mail \u{2014} inbox".into(), url: "https://mail.test/?a=1&b=2".into(), added: 0 });
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(b.other.len(), 2);
        assert_eq!(b.counts(), (4, 2));
    }

    #[test]
    fn export_round_trips() {
        let mut tree = Tree::default();
        tree.add("A <b> & \"q\"", "https://a.test/?x=1&y=2");
        let folder = tree.create_folder(crate::bookmarks::OTHER_ID, "Deep & dark").unwrap();
        let sub = tree.create_folder(folder, "Inner").unwrap();
        tree.add("B", "https://b.test/");
        let b = tree.bar.children.last().unwrap().id;
        tree.move_to(b, sub);
        let html = export(&tree);
        assert!(html.contains("&lt;b&gt;") && !html.contains("<b>"));
        let back = parse(&html).unwrap();
        assert_eq!(back.bar_title, crate::bookmarks::BAR_TITLE);
        assert_eq!(back.bar.len(), 1);
        assert!(matches!(&back.bar[0], ImportItem::Link { title, url, .. } if title == "A <b> & \"q\"" && url == "https://a.test/?x=1&y=2"));
        let mut fresh = Tree::default();
        let stats = crate::bookmarks::merge_import(&mut fresh, &back, &crate::bookmarks::Destination::Merge, "html");
        assert_eq!(stats.added, 2);
        assert_eq!(fresh.entries().iter().map(|e| e.folder.clone()).collect::<Vec<_>>(), [
            "Bookmarks bar".to_string(),
            "Other bookmarks \u{203a} Deep & dark \u{203a} Inner".to_string()
        ]);
    }

    #[test]
    fn malformed_and_hostile_input() {
        assert!(parse("just some text").is_err());
        assert!(parse("").is_err());
        // Unclosed tags, missing </A>, a truncated file.
        let b = parse("<DL><p><DT><H3>Open<DL><DT><A HREF=\"https://x.test/\">X<DT><A HREF=https://y.test/>Y</A><DT><A>no href</A>").unwrap();
        assert_eq!(b.counts().0, 2, "{b:?}");
        // Very deep nesting is flattened, not recursed.
        let deep = format!("<DL>{}<DT><A HREF=\"https://deep.test/\">D</A>{}</DL>", "<DT><H3>f</H3><DL>".repeat(5000), "</DL>".repeat(5000));
        let b = parse(&deep).unwrap();
        assert_eq!(b.counts().0, 1);
        assert!(b.counts().1 <= MAX_DEPTH + 1);
        // Too many items.
        let many = format!("<DL>{}</DL>", "<DT><A HREF=\"https://m.test/\">m</A>".repeat(MAX_ITEMS + 1));
        assert!(parse(&many).is_err());
        assert_eq!(decode_entities("&bogus; &#xZZ; &#128512; &"), "&bogus; &#xZZ; \u{1F600} &");
    }

    #[test]
    fn firefox_export_with_toolbar_inside_the_menu() {
        // Firefox puts the menu items at the top level and the toolbar as a
        // marked folder among them.
        let text = r#"<DL><p>
            <DT><A HREF="https://menu.test/">Menu item</A>
            <DT><H3 PERSONAL_TOOLBAR_FOLDER="true">Bookmarks Toolbar</H3>
            <DL><p><DT><A HREF="https://bar.test/">Bar item</A></DL><p>
            <DT><H3 UNFILED_BOOKMARKS_FOLDER="true">Other Bookmarks</H3>
            <DL><p><DT><A HREF="https://unfiled.test/">Unfiled</A></DL><p>
        </DL>"#;
        let b = parse(text).unwrap();
        assert_eq!(b.bar_title, "Bookmarks Toolbar");
        assert_eq!(b.bar.len(), 1);
        assert_eq!(b.other.len(), 2);
    }
}
