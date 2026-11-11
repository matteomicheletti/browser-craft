//! Headless mode on WebKit: `browsecraft headless <command> <url> …` loads
//! the page in an offscreen GTK window with WebKitGTK (JavaScript and all),
//! does one thing and prints one JSON object on stdout:
//!
//! - `screenshot <url> <out.png> [--width W] [--height H] [--full-page]`
//! - `pdf <url> <out.pdf>`
//! - `text <url>`, `snapshot <url>`, `eval <url> <js>`
//!
//! Options for all: `--timeout S` (load + work, default 30), `--delay MS`
//! (wait after load, default 0), `--user-agent PRESET|STRING` (one
//! User-Agent for every site: default, safari, chrome, firefox, edge or a
//! string; without it the browser's per-site choices apply). Exit codes: 0 done, 1 failed (the JSON
//! says why), 2 bad usage.
//!
//! Each run uses an ephemeral WebKit context (no cookies or cache kept) and
//! WebKit's bubblewrap sandbox, as the browser does. It needs a display
//! (X11 or Wayland); on a server, run it under `xvfb-run` or a headless
//! Wayland compositor.
//!
//! `snapshot_png` and `print_pdf` also serve the browser's `page.screenshot`
//! and `page.pdf` (through wry's `WebViewExtUnix::webview()`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use base64::Engine;
use gtk::prelude::*;
use gtk::{gio, glib};
use image::ImageEncoder;
use javascriptcore::ValueExt;
use serde_json::{Value, json};
use webkit2gtk::{
    LoadEvent, PrintOperation, PrintOperationExt, SettingsExt, SnapshotOptions, SnapshotRegion,
    URIResponseExt, UserContentInjectedFrames, UserContentManager, UserContentManagerExt, UserScript,
    UserScriptInjectionTime, WebContext, WebResourceExt, WebView, WebViewExt,
};

use crate::automation::console::console_capture_script;
use crate::automation::protocol::Method;
use crate::automation::scripts::{self, PageJob, Step};

const USAGE: &str = r##"usage:
  browsecraft headless screenshot <url> <out.png> [--width W] [--height H] [--full-page]
  browsecraft headless pdf <url> <out.pdf>
  browsecraft headless text <url>
  browsecraft headless snapshot <url>
  browsecraft headless eval <url> <javascript>
  browsecraft headless run <url> <steps-json>
steps: [{"method":"page.type","params":{"selector":"#q","text":"hi","submit":true}}, …]
       with page.text, html, snapshot, click, type, press, scroll, eval, wait
options: --timeout SECONDS (default 30), --delay MS after load (default 0),
         --user-agent default|safari|chrome|firefox|edge|STRING (every site)
Prints one JSON object on stdout; exit code 0 on success, 1 on failure.
"##;

/// Largest viewport side, in pixels.
const MAX_SIDE: i32 = 8192;

#[derive(Clone, Debug, PartialEq)]
enum Command {
    Screenshot { out: PathBuf, full_page: bool },
    Pdf { out: PathBuf },
    Text,
    Snapshot,
    Eval { script: String },
    /// A sequence of `page.*` script methods, run in order.
    Run { steps: Vec<(Method, Value)> },
}

#[derive(Clone, Debug, PartialEq)]
struct Options {
    command: Command,
    url: String,
    width: i32,
    height: i32,
    timeout: Duration,
    delay: Duration,
    user_agent: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut positional = Vec::new();
    let (mut width, mut height, mut full_page) = (1280, 800, false);
    let (mut timeout, mut delay) = (Duration::from_secs(30), Duration::ZERO);
    let mut user_agent = None;
    let mut it = args.iter();
    let num = |v: Option<&String>, name: &str, max: u64| -> Result<u64, String> {
        v.and_then(|s| s.parse::<u64>().ok())
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| format!("{name} needs a number from 1 to {max}"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--width" => width = num(it.next(), "--width", MAX_SIDE as u64)? as i32,
            "--height" => height = num(it.next(), "--height", MAX_SIDE as u64)? as i32,
            "--full-page" => full_page = true,
            "--timeout" => timeout = Duration::from_secs(num(it.next(), "--timeout", 600)?),
            "--delay" => delay = Duration::from_millis(num(it.next(), "--delay", 60_000)?),
            "--user-agent" => user_agent = Some(it.next().ok_or("--user-agent needs a value")?.clone()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => positional.push(arg.clone()),
        }
    }
    let mut p = positional.into_iter();
    let (Some(cmd), Some(url)) = (p.next(), p.next()) else {
        return Err("a command and a URL are required".into());
    };
    let command = match (cmd.as_str(), p.next()) {
        ("screenshot", Some(out)) => Command::Screenshot { out: out.into(), full_page },
        ("pdf", Some(out)) => Command::Pdf { out: out.into() },
        ("text", None) => Command::Text,
        ("snapshot", None) => Command::Snapshot,
        ("eval", Some(script)) => Command::Eval { script },
        ("run", Some(steps)) => Command::Run { steps: parse_steps(&steps)? },
        _ => return Err(format!("bad arguments for {cmd}")),
    };
    if p.next().is_some() {
        return Err("too many arguments".into());
    }
    Ok(Options { command, url: page_url(&url), width, height, timeout, delay, user_agent })
}

/// The `run` steps: a JSON array of `{method, params}` whose methods are
/// answered by page scripts. Each step is validated up front.
fn parse_steps(text: &str) -> Result<Vec<(Method, Value)>, String> {
    let steps: Vec<Value> = serde_json::from_str(text).map_err(|e| format!("steps must be a JSON array: {e}"))?;
    if steps.is_empty() || steps.len() > 100 {
        return Err("give 1 to 100 steps".into());
    }
    steps
        .into_iter()
        .enumerate()
        .map(|(i, step)| {
            let name = step.get("method").and_then(Value::as_str).unwrap_or_default();
            let method = Method::from_name(name).ok_or_else(|| format!("step {i}: unknown method {name:?}"))?;
            let params = step.get("params").cloned().unwrap_or(Value::Null);
            match PageJob::start(method, &params) {
                Ok(Some(_)) => Ok((method, params)),
                Ok(None) => Err(format!("step {i}: {method} is not available in headless run")),
                Err(e) => Err(format!("step {i}: {}", e.message)),
            }
        })
        .collect()
}

/// A local file path becomes a `file://` URL; anything else is resolved
/// like address-bar input.
fn page_url(input: &str) -> String {
    let path = Path::new(input);
    if !input.contains("://")
        && path.exists()
        && let Some(url) = std::fs::canonicalize(path).ok().and_then(|p| url::Url::from_file_path(p).ok())
    {
        return url.to_string();
    }
    crate::address::resolve(input)
}

/// The `headless` command; returns the exit code.
pub fn main(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return 0;
    }
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprint!("browsecraft headless: {e}\n{USAGE}");
            return 2;
        }
    };
    crate::useragent::load();
    if let Some(ua) = &opts.user_agent
        && let Err(e) = crate::useragent::force(ua)
    {
        eprint!("browsecraft headless: {e}\n{USAGE}");
        return 2;
    }
    match run(&opts) {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            println!("{}", json!({ "ok": false, "url": opts.url, "error": e }));
            eprintln!("browsecraft headless: {e}");
            1
        }
    }
}

/// The environment `linux_gtk_init` sets for the browser: WebKit's sandbox
/// on, the DMA-BUF renderer off. The GDK backend is left to GTK, since an
/// offscreen window needs no X11 embedding.
fn webkit_env() {
    // SAFETY: called on the main thread before any other thread is spawned
    // (the headless command runs before the browser starts).
    unsafe {
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        if std::env::var_os("BROWSECRAFT_NO_SANDBOX").is_none() {
            std::env::set_var("WEBKIT_FORCE_SANDBOX", "1");
        } else {
            eprintln!("browsecraft: BROWSECRAFT_NO_SANDBOX is set: web content is NOT sandboxed");
        }
    }
}

type Finish = Rc<dyn Fn(Result<Value, String>)>;

fn run(opts: &Options) -> Result<Value, String> {
    webkit_env();
    gtk::init().map_err(|e| format!("GTK cannot start ({e}); a display is needed (try xvfb-run)"))?;

    let context = WebContext::new_ephemeral();
    let content = UserContentManager::new();
    content.add_script(&UserScript::new(
        &console_capture_script(),
        UserContentInjectedFrames::TopFrame,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    let webview = WebView::builder().web_context(&context).user_content_manager(&content).build();
    if let Some(settings) = WebViewExt::settings(&webview) {
        settings.set_enable_write_console_messages_to_stdout(false);
        settings.set_enable_developer_extras(false);
    }
    // The browser's User-Agent and per-site choices, as in a tab.
    crate::useragent::webkit::attach(&webview, |_| true);
    let window = gtk::OffscreenWindow::new();
    window.set_default_size(opts.width, opts.height);
    webview.set_size_request(opts.width, opts.height);
    window.add(&webview);
    window.show_all();

    let main_loop = glib::MainLoop::new(None, false);
    let outcome: Rc<RefCell<Option<Result<Value, String>>>> = Rc::new(RefCell::new(None));
    let finish: Finish = {
        let (outcome, main_loop) = (outcome.clone(), main_loop.clone());
        Rc::new(move |r| {
            let mut slot = outcome.borrow_mut();
            if slot.is_none() {
                *slot = Some(r);
                main_loop.quit();
            }
        })
    };

    let started = Rc::new(RefCell::new(false));
    {
        let finish = finish.clone();
        webview.connect_load_failed(move |_, _, uri, error| {
            // A load replaced by another (redirect, script navigation) is
            // not a failure.
            // Nor one the User-Agent switch dropped to load again.
            if !error.matches(webkit2gtk::NetworkError::Cancelled)
                && !error.matches(webkit2gtk::PolicyError::FrameLoadInterruptedByPolicyChange)
            {
                finish(Err(format!("cannot load {uri}: {error}")));
            }
            false
        });
    }
    {
        let (finish, command, delay) = (finish.clone(), opts.command.clone(), opts.delay);
        let timeout_ms = opts.timeout.as_millis() as u64;
        webview.connect_load_changed(move |wv, event| {
            if event != LoadEvent::Finished || started.replace(true) {
                return;
            }
            let (wv, finish, command) = (wv.clone(), finish.clone(), command.clone());
            glib::timeout_add_local_once(delay, move || act(&wv, &command, timeout_ms, finish));
        });
    }
    {
        let finish = finish.clone();
        let secs = opts.timeout.as_secs();
        glib::timeout_add_local_once(opts.timeout, move || finish(Err(format!("timed out after {secs} s"))));
    }
    webview.load_uri(&opts.url);
    main_loop.run();

    let result = outcome.borrow_mut().take().unwrap_or_else(|| Err("stopped".into()));
    // SAFETY: the window is ours and no longer used.
    unsafe { window.destroy() };
    result
}

/// Does the command on the loaded page, then adds the page's details and
/// console messages to the result.
fn act(wv: &WebView, command: &Command, timeout_ms: u64, finish: Finish) {
    let with_details: Finish = {
        let wv = wv.clone();
        Rc::new(move |r: Result<Value, String>| match r {
            Err(e) => finish(Err(e)),
            Ok(mut v) => {
                let finish = finish.clone();
                let page = page_info(&wv);
                run_script(&wv, scripts::console_buffer_script(), move |json| {
                    let console = match scripts::parse_script_result(&json) {
                        Ok(scripts::ScriptOutcome::Done(c)) => c,
                        _ => json!([]),
                    };
                    if let (Some(obj), Some(info)) = (v.as_object_mut(), page.as_object()) {
                        let mut out = serde_json::Map::new();
                        out.insert("ok".into(), json!(true));
                        out.extend(info.clone());
                        out.extend(std::mem::take(obj));
                        out.insert("console".into(), console);
                        v = Value::Object(out);
                    }
                    finish(Ok(v));
                });
            }
        })
    };
    match command {
        Command::Screenshot { out, full_page } => {
            let out = out.clone();
            snapshot_png(wv, *full_page, move |r| {
                with_details(r.and_then(|(png, w, h)| {
                    std::fs::write(&out, &png).map_err(|e| format!("{}: {e}", out.display()))?;
                    Ok(json!({ "path": out.display().to_string(), "width": w, "height": h, "bytes": png.len() }))
                }))
            });
        }
        Command::Pdf { out } => {
            let out = absolute(out);
            let path = out.display().to_string();
            print_pdf(wv, &out, move |r| {
                with_details(r.map(|bytes| json!({ "path": path, "bytes": bytes })))
            });
        }
        Command::Text => start_job(wv, Method::PageText, json!({ "max_chars": 2_000_000 }), with_details),
        Command::Snapshot => start_job(wv, Method::PageSnapshot, json!({ "max_elements": 5000 }), with_details),
        Command::Eval { script } => {
            start_job(wv, Method::PageEval, json!({ "script": script, "timeout_ms": timeout_ms }), with_details)
        }
        Command::Run { steps } => run_steps(wv.clone(), steps.clone(), Vec::new(), with_details),
    }
}

/// Runs the steps in order; stops at the first error, naming the step.
fn run_steps(wv: WebView, mut steps: Vec<(Method, Value)>, mut results: Vec<Value>, finish: Finish) {
    if steps.is_empty() {
        return finish(Ok(json!({ "results": results })));
    }
    let (method, params) = steps.remove(0);
    let index = results.len();
    let next: Finish = {
        let wv = wv.clone();
        let steps = RefCell::new(Some(steps));
        let results = RefCell::new(Some(std::mem::take(&mut results)));
        Rc::new(move |r: Result<Value, String>| {
            let (Some(steps), Some(mut results)) = (steps.take(), results.take()) else { return };
            match r {
                Ok(v) => {
                    results.push(json!({ "method": method.name(), "result": v }));
                    run_steps(wv.clone(), steps, results, finish.clone());
                }
                Err(e) => finish(Err(format!("step {index} ({method}): {e}"))),
            }
        })
    };
    start_job(&wv, method, params, next);
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// URL, title and HTTP status of the loaded page.
fn page_info(wv: &WebView) -> Value {
    let status = wv.main_resource().and_then(|r| r.response()).map(|r| r.status_code()).filter(|s| *s != 0);
    json!({
        "url": wv.uri().map(|u| u.to_string()).unwrap_or_default(),
        "title": wv.title().map(|t| t.to_string()).unwrap_or_default(),
        "status": status,
    })
}

/// Runs `script` and passes its completion value as JSON, or an empty
/// string when it threw or did not parse (as wry's
/// `evaluate_script_with_callback` does).
pub fn run_script(wv: &WebView, script: &str, done: impl FnOnce(String) + 'static) {
    wv.evaluate_javascript(script, None, None, None::<&gio::Cancellable>, move |r| {
        let json = r
            .ok()
            .and_then(|v| v.to_json(0))
            .map(|s| s.to_string())
            .unwrap_or_default();
        done(json);
    });
}

fn start_job(wv: &WebView, method: Method, params: Value, finish: Finish) {
    match PageJob::start(method, &params) {
        Ok(Some((job, script))) => drive(wv.clone(), job, script, finish),
        Ok(None) => finish(Err(format!("{method} is not a page script"))),
        Err(e) => finish(Err(e.message)),
    }
}

/// Runs a page job's scripts until it is done.
fn drive(wv: WebView, mut job: PageJob, script: String, finish: Finish) {
    let wv2 = wv.clone();
    run_script(&wv, &script, move |json| match job.on_result(&json) {
        Step::Run(next) => drive(wv2, job, next, finish),
        Step::RunLater(next) => {
            glib::timeout_add_local_once(scripts::POLL_INTERVAL, move || drive(wv2, job, next, finish));
        }
        Step::Done(r) => finish(r.map_err(|e| e.message)),
    });
}

/// A PNG of the page (the visible part, or the whole document) with its
/// width and height. Must run on the GTK main thread.
pub fn snapshot_png(wv: &WebView, full_page: bool, done: impl FnOnce(Result<(Vec<u8>, u32, u32), String>) + 'static) {
    let region = if full_page { SnapshotRegion::FullDocument } else { SnapshotRegion::Visible };
    wv.snapshot(region, SnapshotOptions::NONE, None::<&gio::Cancellable>, move |r| {
        done(r.map_err(|e| format!("snapshot failed: {e}")).and_then(|surface| surface_png(&surface)));
    });
}

/// Encodes a cairo ARGB32/RGB24 image surface (premultiplied, native byte
/// order) as PNG.
pub fn surface_png(surface: &gtk::cairo::Surface) -> Result<(Vec<u8>, u32, u32), String> {
    let image = gtk::cairo::ImageSurface::try_from(surface.clone()).map_err(|_| "the snapshot is not an image")?;
    let (w, h, stride) = (image.width().max(0) as usize, image.height().max(0) as usize, image.stride().max(0) as usize);
    let opaque = image.format() == gtk::cairo::Format::Rgb24;
    if !matches!(image.format(), gtk::cairo::Format::ARgb32 | gtk::cairo::Format::Rgb24) {
        return Err(format!("unsupported snapshot format {:?}", image.format()));
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    image
        .with_data(|data| {
            for row in data.chunks(stride).take(h) {
                for px in row.as_chunks::<4>().0.iter().take(w) {
                    let argb = u32::from_ne_bytes(*px);
                    let a = if opaque { 255 } else { (argb >> 24) as u8 };
                    let un = |c: u32| -> u8 {
                        let c = (c & 0xff) as u16;
                        match a {
                            0 => 0,
                            255 => c as u8,
                            a => ((c * 255 + a as u16 / 2) / a as u16).min(255) as u8,
                        }
                    };
                    rgba.extend_from_slice(&[un(argb >> 16), un(argb >> 8), un(argb), a]);
                }
            }
        })
        .map_err(|e| format!("snapshot data: {e}"))?;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encoding: {e}"))?;
    Ok((png, w as u32, h as u32))
}

/// Prints the page to a PDF file at `out` (absolute) without a dialog;
/// passes the file's size. Must run on the GTK main thread.
pub fn print_pdf(wv: &WebView, out: &Path, done: impl FnOnce(Result<u64, String>) + 'static) {
    let uri = match url::Url::from_file_path(out) {
        Ok(u) => u.to_string(),
        Err(()) => return done(Err(format!("{} is not an absolute path", out.display()))),
    };
    let op = PrintOperation::new(wv);
    let settings = gtk::PrintSettings::new();
    settings.set_printer("Print to File");
    settings.set("output-file-format", Some("pdf"));
    settings.set("output-uri", Some(&uri));
    op.set_print_settings(&settings);
    op.set_page_setup(&gtk::PageSetup::new());
    let done = Rc::new(RefCell::new(Some(done)));
    {
        let done = done.clone();
        op.connect_failed(move |_, e| {
            if let Some(d) = done.borrow_mut().take() {
                d(Err(format!("printing failed: {e}")));
            }
        });
    }
    let out = out.to_path_buf();
    op.connect_finished(move |_| {
        if let Some(d) = done.borrow_mut().take() {
            d(std::fs::metadata(&out)
                .map(|m| m.len())
                .map_err(|e| format!("{}: {e}", out.display()))
                .and_then(|n| if n > 0 { Ok(n) } else { Err("the PDF is empty".into()) }));
        }
    });
    op.print();
}

/// Base64 of a file, for `page.pdf`'s result.
#[allow(dead_code)] // used by the browser's page.pdf once wired
pub fn file_base64(path: &Path) -> Result<String, String> {
    std::fs::read(path)
        .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn automation_headless_args() {
        let o = parse_args(&args(&["screenshot", "https://a.test/", "out.png", "--width", "640", "--full-page"])).unwrap();
        assert_eq!(o.command, Command::Screenshot { out: "out.png".into(), full_page: true });
        assert_eq!((o.width, o.height, o.url.as_str()), (640, 800, "https://a.test/"));
        let o = parse_args(&args(&["eval", "example.com", "document.title", "--delay", "500", "--timeout", "5"])).unwrap();
        assert_eq!(o.command, Command::Eval { script: "document.title".into() });
        assert_eq!((o.url.as_str(), o.delay, o.timeout), ("https://example.com", Duration::from_millis(500), Duration::from_secs(5)));
        assert_eq!(parse_args(&args(&["text", "https://a.test"])).unwrap().command, Command::Text);
        let o = parse_args(&args(&["text", "https://a.test", "--user-agent", "chrome"])).unwrap();
        assert_eq!(o.user_agent.as_deref(), Some("chrome"));
        assert!(parse_args(&args(&["text", "https://a.test", "--user-agent"])).is_err());
        assert!(parse_args(&args(&["text"])).is_err());
        assert!(parse_args(&args(&["text", "u", "extra"])).is_err());
        assert!(parse_args(&args(&["pdf", "u"])).is_err());
        assert!(parse_args(&args(&["screenshot", "u", "o", "--width", "0"])).is_err());
        assert!(parse_args(&args(&["screenshot", "u", "o", "--width", "99999"])).is_err());
        assert!(parse_args(&args(&["text", "u", "--bogus"])).is_err());
        assert!(parse_args(&args(&["fly", "u"])).is_err());
        let o = parse_args(&args(&["run", "u", r##"[{"method":"page.click","params":{"selector":"#a"}},{"method":"page.text"}]"##])).unwrap();
        assert!(matches!(o.command, Command::Run { ref steps } if steps.len() == 2 && steps[1].0 == Method::PageText));
        assert!(parse_args(&args(&["run", "u", r#"[{"method":"tabs.list"}]"#])).unwrap_err().contains("not available"));
        assert!(parse_args(&args(&["run", "u", r#"[{"method":"page.click","params":{}}]"#])).unwrap_err().contains("step 0"));
        assert!(parse_args(&args(&["run", "u", "[]"])).is_err());
        let here = std::env::current_dir().unwrap().join("Cargo.toml");
        assert!(page_url(here.to_str().unwrap()).starts_with("file:///"));
    }
}
