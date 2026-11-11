//! Console capture: a page script that reports `console.*` calls, uncaught
//! errors and unhandled promise rejections to the browser through
//! `window.ipc.postMessage('bc-console:' + json)`, the parser for those
//! messages, and the per-tab ring buffer the browser keeps. The same buffer
//! feeds `console.read` and the developer JS console.
//!
//! The page is untrusted: it can post fake messages with this prefix, so the
//! entries are only ever displayed or returned as data, never acted on.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// The IPC prefix this module owns (docs/CONTRACTS.md).
pub const PREFIX: &str = "bc-console:";
/// Largest message accepted from the page.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// Longest text kept per entry (the page script truncates at the same size).
pub const MAX_TEXT_CHARS: usize = 4000;
/// Entries kept per tab.
pub const DEFAULT_CAPACITY: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Log,
    Info,
    Warn,
    Error,
}

/// A message from the page, as parsed from IPC.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PageConsoleMessage {
    pub level: Level,
    pub text: String,
    /// `console`, `error` (window.onerror) or `rejection`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
    /// Messages the page-side rate limit dropped before this one.
    #[serde(default)]
    pub dropped: u64,
}

/// One entry of a tab's console log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub seq: u64,
    /// Milliseconds since the Unix epoch, taken by the browser.
    pub time_ms: u64,
    pub level: Level,
    pub kind: String,
    pub text: String,
    /// The page URL at the time (the sender of the IPC message).
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// Parses a `bc-console:` IPC message; None for other prefixes, oversized or
/// malformed messages.
pub fn parse_page_message(body: &str) -> Option<PageConsoleMessage> {
    if body.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let json = body.strip_prefix(PREFIX)?;
    let mut msg: PageConsoleMessage = serde_json::from_str(json).ok()?;
    truncate_chars(&mut msg.text, MAX_TEXT_CHARS);
    if let Some(source) = msg.source.as_mut() {
        truncate_chars(source, 2048);
    }
    truncate_chars(&mut msg.kind, 16);
    Some(msg)
}

fn truncate_chars(s: &mut String, max: usize) {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
    }
}

/// A tab's console log: a ring buffer with monotonically increasing
/// sequence numbers, so readers can poll with `since`.
#[derive(Clone, Debug)]
pub struct ConsoleLog {
    entries: VecDeque<ConsoleEntry>,
    capacity: usize,
    next_seq: u64,
    dropped: u64,
}

impl Default for ConsoleLog {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl ConsoleLog {
    pub fn new(capacity: usize) -> Self {
        Self { entries: VecDeque::new(), capacity: capacity.max(1), next_seq: 0, dropped: 0 }
    }

    /// Adds a message the page at `url` posted. Returns its sequence number.
    pub fn push(&mut self, msg: PageConsoleMessage, url: &str) -> u64 {
        let time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.dropped = self.dropped.saturating_add(msg.dropped);
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let mut url = url.to_string();
        truncate_chars(&mut url, 2048);
        self.entries.push_back(ConsoleEntry {
            seq,
            time_ms,
            level: msg.level,
            kind: if msg.kind.is_empty() { "console".into() } else { msg.kind },
            text: msg.text,
            url,
            source: msg.source,
            line: msg.line,
            column: msg.column,
        });
        seq
    }

    /// Entries from `since` on, at `min_level` or more severe, at most `limit`
    /// (the oldest first). Returns them with the `since` to use next time.
    pub fn read(&self, since: u64, min_level: Option<Level>, limit: usize) -> (Vec<ConsoleEntry>, u64) {
        let mut next = since.max(self.entries.front().map_or(self.next_seq, |e| e.seq));
        let mut out = Vec::new();
        for e in self.entries.iter().filter(|e| e.seq >= since) {
            if out.len() >= limit {
                break;
            }
            next = e.seq + 1;
            if min_level.is_none_or(|min| e.level >= min) {
                out.push(e.clone());
            }
        }
        (out, next)
    }

    /// Answers `console.read` from this log.
    pub fn read_result(&self, params: &crate::automation::protocol::ConsoleReadParams) -> crate::automation::protocol::ConsoleReadResult {
        let limit = params.limit.unwrap_or(200).clamp(1, self.capacity);
        let (entries, next) = self.read(params.since.unwrap_or(0), params.level, limit);
        crate::automation::protocol::ConsoleReadResult { entries, next, dropped: self.dropped }
    }

    pub fn entries(&self) -> impl Iterator<Item = &ConsoleEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Empties the log (the sequence keeps counting, so readers notice).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The page script: wraps `console.debug/log/info/warn/error`, listens for
/// `error` and `unhandledrejection`, and posts each as a `bc-console:`
/// message. At most 50 messages a second leave the page (the rest are
/// counted and reported as `dropped`), each truncated to 4000 characters.
/// The last 200 are also kept in the page (`window.__bcConsoleBuf`) for the
/// headless mode, which has no IPC channel.
pub fn console_capture_script() -> String {
    CAPTURE_SCRIPT
        .replace("__MAX_TEXT__", &MAX_TEXT_CHARS.to_string())
        .replace("__PREFIX__", &serde_json::to_string(PREFIX).unwrap_or_else(|_| "\"bc-console:\"".into()))
}

const CAPTURE_SCRIPT: &str = r#"(function () {
  'use strict';
  if (window.__bcConsoleHooked) return;
  Object.defineProperty(window, '__bcConsoleHooked', { value: true });
  var MAX = __MAX_TEXT__, PER_SECOND = 50, KEEP = 200;
  var buf = [];
  Object.defineProperty(window, '__bcConsoleBuf', { value: buf });
  var windowStart = 0, sent = 0, dropped = 0;
  var post = (window.ipc && window.ipc.postMessage) ? window.ipc.postMessage.bind(window.ipc) : null;
  function fmt(v) {
    try {
      if (typeof v === 'string') return v;
      if (v instanceof Error) return (v.stack || (v.name + ': ' + v.message));
      if (v === undefined) return 'undefined';
      if (typeof v === 'function') return 'function ' + (v.name || '');
      if (v instanceof Node) return '<' + (v.nodeName || 'node').toLowerCase() + '>';
      var s = JSON.stringify(v);
      return s === undefined ? String(v) : s;
    } catch (e) { try { return String(v); } catch (e2) { return '[unprintable]'; } }
  }
  function send(level, kind, text, source, line, column) {
    var now = Date.now();
    if (now - windowStart >= 1000) { windowStart = now; sent = 0; }
    if (sent >= PER_SECOND) { dropped++; return; }
    sent++;
    if (text.length > MAX) text = text.slice(0, MAX);
    var msg = { level: level, kind: kind, text: text, dropped: dropped };
    if (source) msg.source = String(source).slice(0, 2048);
    if (line) msg.line = line >>> 0;
    if (column) msg.column = column >>> 0;
    dropped = 0;
    buf.push(msg); if (buf.length > KEEP) buf.shift();
    if (post) { try { post(__PREFIX__ + JSON.stringify(msg)); } catch (e) {} }
  }
  ['debug', 'log', 'info', 'warn', 'error'].forEach(function (level) {
    var original = console[level];
    if (typeof original !== 'function') return;
    console[level] = function () {
      try {
        var parts = [];
        for (var i = 0; i < arguments.length && i < 20; i++) parts.push(fmt(arguments[i]));
        send(level, 'console', parts.join(' '));
      } catch (e) {}
      return original.apply(this, arguments);
    };
  });
  window.addEventListener('error', function (e) {
    if (e.target && e.target !== window) {
      var t = e.target, src = t.src || t.href || '';
      send('error', 'resource', 'Failed to load <' + (t.tagName || '').toLowerCase() + '> ' + src, src);
      return;
    }
    var text = e.message || 'Script error';
    if (e.error && e.error.stack) text += '\n' + e.error.stack;
    send('error', 'error', text, e.filename, e.lineno, e.colno);
  }, true);
  window.addEventListener('unhandledrejection', function (e) {
    send('error', 'rejection', 'Unhandled promise rejection: ' + fmt(e.reason));
  });
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(level: Level, text: &str) -> PageConsoleMessage {
        PageConsoleMessage {
            level,
            text: text.into(),
            kind: "console".into(),
            source: None,
            line: None,
            column: None,
            dropped: 0,
        }
    }

    #[test]
    fn automation_console_messages_parse_defensively() {
        let m = parse_page_message(r#"bc-console:{"level":"warn","kind":"console","text":"hi","dropped":2}"#).unwrap();
        assert_eq!((m.level, m.text.as_str(), m.dropped), (Level::Warn, "hi", 2));
        let m = parse_page_message(r#"bc-console:{"level":"error","kind":"error","text":"x","source":"https://a.test/a.js","line":3,"column":9}"#).unwrap();
        assert_eq!((m.line, m.column, m.source.as_deref()), (Some(3), Some(9), Some("https://a.test/a.js")));
        assert!(parse_page_message(r#"bc:forms?kind=login"#).is_none());
        assert!(parse_page_message(r#"bc-console:{"level":"fatal","text":"x"}"#).is_none());
        assert!(parse_page_message(r#"bc-console:not json"#).is_none());
        assert!(parse_page_message(&format!(r#"bc-console:{{"level":"log","text":"{}"}}"#, "a".repeat(MAX_MESSAGE_BYTES))).is_none());
        let long = "é".repeat(MAX_TEXT_CHARS + 10);
        let m = parse_page_message(&format!(r#"bc-console:{{"level":"log","text":"{long}"}}"#)).unwrap();
        assert_eq!(m.text.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn automation_console_ring_buffer() {
        let mut log = ConsoleLog::new(3);
        for i in 0..5 {
            log.push(msg(if i % 2 == 0 { Level::Log } else { Level::Error }, &format!("m{i}")), "https://a.test/");
        }
        assert_eq!(log.len(), 3);
        let (all, next) = log.read(0, None, 100);
        assert_eq!(all.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), ["m2", "m3", "m4"]);
        assert_eq!(next, 5);
        let (errors, _) = log.read(0, Some(Level::Warn), 100);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].text, "m3");
        let (none, next2) = log.read(next, None, 100);
        assert!(none.is_empty());
        assert_eq!(next2, 5);
        let (first, next3) = log.read(0, None, 1);
        assert_eq!((first.len(), next3), (1, 3));
        let r = log.read_result(&Default::default());
        assert_eq!((r.entries.len(), r.dropped, r.next), (3, 2, 5));
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.push(msg(Level::Info, "after"), "u"), 5);
    }

    #[test]
    fn automation_console_script_is_complete() {
        let s = console_capture_script();
        assert!(s.contains("\"bc-console:\"") && s.contains("unhandledrejection") && s.contains("PER_SECOND = 50"));
        assert!(!s.contains("__MAX_TEXT__") && !s.contains("__PREFIX__"));
    }
}
