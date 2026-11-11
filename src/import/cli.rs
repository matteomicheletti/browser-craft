//! `browsecraft import`: imports from a terminal and prints a JSON summary.
//!
//! ```text
//! browsecraft import --list
//! browsecraft import firefox|chrome|chromium|brave|edge|vivaldi|opera|librewolf|waterfox
//!     [--profile NAME|PATH] [--bookmarks] [--history] [--days N] [--merge] [--dry-run]
//! browsecraft import html --file F [--merge] [--dry-run]
//! browsecraft import --export [DIR]
//! browsecraft import --undo
//! ```
//!
//! It may run while the browser is open: history goes through SQLite (WAL,
//! a busy timeout), bookmarks.json is changed under its lock file and the
//! browser reloads it within a couple of seconds.

use std::path::PathBuf;
use std::sync::Arc;

use super::{Options, Source, detect};

const USAGE: &str = "usage:
  browsecraft import --list
  browsecraft import <browser> [--profile NAME|PATH] [--bookmarks] [--history] [--days N] [--merge] [--dry-run]
      browser: firefox, librewolf, waterfox, chrome, chrome-beta, chrome-dev, chromium, brave, edge, vivaldi, opera
  browsecraft import html --file FILE [--merge] [--dry-run]
  browsecraft import --export [DIR]
  browsecraft import --undo
Without --bookmarks or --history, both are imported. Passwords, cookies and extensions are never imported.";

fn print_json(value: &impl serde::Serialize) -> i32 {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("browsecraft: {e}");
            1
        }
    }
}

/// Parsed arguments.
#[derive(Debug, Default, PartialEq)]
struct Args {
    browser: Option<String>,
    profile: Option<String>,
    file: Option<PathBuf>,
    options: Options,
    list: bool,
    undo: bool,
    export: Option<Option<PathBuf>>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { options: Options { bookmarks: false, history: false, ..Options::default() }, ..Default::default() };
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--list" => a.list = true,
            "--undo" => a.undo = true,
            "--export" => a.export = Some(None),
            "--profile" => a.profile = Some(value("--profile")?),
            "--file" => a.file = Some(PathBuf::from(value("--file")?)),
            "--days" => {
                let days = value("--days")?.parse::<u32>().map_err(|_| "--days needs a number of days".to_string())?;
                a.options.days = Some(days).filter(|d| *d > 0);
            }
            "--bookmarks" => a.options.bookmarks = true,
            "--history" => a.options.history = true,
            "--merge" => a.options.merge = true,
            "--dry-run" => a.options.dry_run = true,
            "-h" | "--help" => return Err(String::new()),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other if a.export == Some(None) => a.export = Some(Some(PathBuf::from(other))),
            other if a.browser.is_none() => a.browser = Some(other.to_ascii_lowercase()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    if !a.options.bookmarks && !a.options.history {
        a.options.bookmarks = true;
        a.options.history = true;
    }
    Ok(a)
}

/// Runs the command; returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("browsecraft: {e}");
            }
            eprintln!("{USAGE}");
            return 2;
        }
    };
    if a.list {
        let list: Vec<serde_json::Value> = detect::detect()
            .into_iter()
            .map(|p| {
                let mut v = serde_json::to_value(&p).unwrap_or_default();
                match super::summary(&p) {
                    Ok(s) => v["summary"] = serde_json::to_value(s).unwrap_or_default(),
                    Err(e) => v["error"] = e.into(),
                }
                v
            })
            .collect();
        return print_json(&list);
    }
    if a.undo {
        return match super::undo_last() {
            Ok(message) => print_json(&serde_json::json!({ "undone": message })),
            Err(e) => {
                eprintln!("browsecraft: {e}");
                1
            }
        };
    }
    if let Some(dir) = a.export {
        let dir = match dir.map(Ok).unwrap_or_else(super::downloads_dir) {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("browsecraft: {e}");
                return 1;
            }
        };
        return match super::export_to(&dir) {
            Ok(path) => print_json(&serde_json::json!({ "exported": path, "bookmarks": crate::bookmarks::snapshot().link_count() })),
            Err(e) => {
                eprintln!("browsecraft: {e}");
                1
            }
        };
    }
    let Some(browser) = a.browser else {
        eprintln!("{USAGE}");
        return 2;
    };
    let source = if browser == "html" {
        let Some(file) = a.file else {
            eprintln!("browsecraft: html needs --file FILE");
            return 2;
        };
        Source::Html(file)
    } else {
        let profiles = detect::detect();
        match detect::pick(&profiles, &browser, a.profile.as_deref()) {
            Ok(p) => Source::Profile(p.clone()),
            Err(e) => {
                eprintln!("browsecraft: {e}");
                return 1;
            }
        }
    };
    let report = super::run(&source, &a.options, Arc::new(|text| eprintln!("{text}")));
    if !a.options.dry_run {
        crate::history::flush();
    }
    let code = print_json(&report);
    if report.errors.is_empty() { code } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options() {
        let a = parse(&args(&["firefox", "--profile", "default-release", "--history", "--days", "30", "--dry-run"])).unwrap();
        assert_eq!(a.browser.as_deref(), Some("firefox"));
        assert_eq!(a.profile.as_deref(), Some("default-release"));
        assert_eq!(a.options, Options { bookmarks: false, history: true, days: Some(30), merge: false, dry_run: true });
        let a = parse(&args(&["Chrome"])).unwrap();
        assert_eq!(a.browser.as_deref(), Some("chrome"));
        assert!(a.options.bookmarks && a.options.history, "both by default");
        let a = parse(&args(&["html", "--file", "/tmp/b.html", "--merge"])).unwrap();
        assert_eq!(a.file, Some(PathBuf::from("/tmp/b.html")));
        assert!(a.options.merge);
        assert_eq!(parse(&args(&["--export", "/tmp/out"])).unwrap().export, Some(Some(PathBuf::from("/tmp/out"))));
        assert_eq!(parse(&args(&["--export"])).unwrap().export, Some(None));
        assert!(parse(&args(&["--days", "x"])).is_err());
        assert!(parse(&args(&["--profile"])).is_err());
        assert!(parse(&args(&["firefox", "chrome"])).is_err());
        assert!(parse(&args(&["--bogus"])).is_err());
        assert_eq!(parse(&args(&["--days", "0", "firefox"])).unwrap().options.days, None);
    }

    #[test]
    fn usage_errors_exit_2() {
        assert_eq!(main(&args(&["--bogus"])), 2);
        assert_eq!(main(&args(&[])), 2);
        assert_eq!(main(&args(&["html"])), 2);
    }
}
