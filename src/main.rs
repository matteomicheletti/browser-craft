#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! BrowseCraft: a small web browser with its own rendering engine.
//!
//! Pipeline: `net` fetches bytes, `html` parses them into a `dom`, `css`
//! parses stylesheets, `style` runs the cascade, `layout` builds a display
//! list and `paint` rasterizes it. `browser` is the window and UI around it.

mod adblock;
mod address;
mod authenticator;
mod automation;
mod bookmarks;
mod browser;
mod capture;
mod commands;
mod containers;
mod css;
mod darkmode;
mod dom;
mod egui_chrome;
mod flags;
mod font;
mod headless;
#[cfg(target_os = "linux")]
mod headless_webkit;
mod history;
mod html;
mod icons;
mod internal;
mod layout;
mod loader;
mod net;
mod paint;
mod palette;
mod passwords;
mod recording;
mod plugins;
mod servo_pane;
mod split;
mod storage;
mod style;
mod tab_groups;
mod textfield;
mod thumbnails;
mod theme;
mod titlebar;
#[cfg(test)]
mod test_http;
mod vault;
#[cfg(windows)]
mod webview_network;

use winit::event_loop::EventLoop;

fn main() {
    // Automation sub-commands (`ctl`, `mcp`, `headless`) run instead of the
    // browser; `--automation` starts it with the control socket on.
    let mut args: Vec<String> = std::env::args().collect();
    if let Some(code) = automation::run_cli(&args) {
        std::process::exit(code);
    }
    automation::take_cli_flag(&mut args);

    flags::load();
    bookmarks::load();
    history::init();
    passwords::load();
    containers::load();
    internal::set_theme(flags::get().theme.resolve(true));
    let fonts = match font::Fonts::new() {
        Ok(fonts) => fonts,
        Err(e) => {
            eprintln!("browsecraft: {e}");
            std::process::exit(1);
        }
    };
    if matches!(args.get(1).map(String::as_str), Some("--version" | "-V")) {
        println!("browsecraft {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.get(1).map(String::as_str) == Some("--screenshot") {
        let (Some(out), Some(url)) = (args.get(2), args.get(3)) else {
            eprintln!("usage: browsecraft --screenshot <out.png> <url> [width]");
            std::process::exit(2);
        };
        let width = match args.get(4) {
            None => 1200,
            Some(w) => match w.parse::<u32>() {
                Ok(w) if (1..=headless::MAX_WIDTH).contains(&w) => w,
                _ => {
                    eprintln!(
                        "browsecraft: the width must be a number from 1 to {}",
                        headless::MAX_WIDTH
                    );
                    std::process::exit(2);
                }
            },
        };
        if let Err(e) = headless::screenshot(&fonts, &address::resolve(url), out, width) {
            eprintln!("browsecraft: {e}");
            std::process::exit(1);
        }
        return;
    }
    let start_tabs = match start_tabs(&args[1..]) {
        Ok(tabs) => tabs,
        Err(e) => {
            eprintln!("browsecraft: {e}");
            std::process::exit(2);
        }
    };
    // Temporary containers of a browser that crashed.
    containers::sweep_stale_temporary();

    #[cfg(target_os = "linux")]
    let gtk_ok = linux_gtk_init();
    let mut builder = EventLoop::with_user_event();
    // wry embeds WebKitGTK as an X11 child window, so the browser window
    // runs on X11 (XWayland under a Wayland session).
    #[cfg(target_os = "linux")]
    if gtk_ok {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let event_loop = match builder.build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("browsecraft: failed to start the event loop: {e}");
            std::process::exit(1);
        }
    };
    let mut app = browser::App::new(event_loop.create_proxy(), fonts, start_tabs);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("browsecraft: {e}");
    }
}

/// The tabs to open from the command line: `[--container NAME |
/// --temporary-container] [URL]...`. Each address opens in a tab, in the
/// container named just before it. Empty: restore the session or go home.
fn start_tabs(args: &[String]) -> Result<Vec<browser::StartTab>, String> {
    use browser::{ContainerChoice, StartTab};
    let mut tabs = Vec::new();
    let mut pending: Option<ContainerChoice> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--container" => {
                let name = iter.next().ok_or("--container needs a container name")?;
                let found = containers::find(name).ok_or_else(|| format!("no container named {name:?}"))?;
                pending = Some(ContainerChoice::Id(found.id));
            }
            "--temporary-container" => pending = Some(ContainerChoice::Temporary),
            url => tabs.push(StartTab {
                url: address::resolve(url),
                container: pending.take().unwrap_or(ContainerChoice::None),
            }),
        }
    }
    if let Some(container) = pending {
        tabs.push(StartTab { url: browser::HOME_URL.to_string(), container });
    }
    Ok(tabs)
}

/// Initialises GTK for WebKitGTK on its X11 backend. Returns false (and the
/// browser falls back to its own engine) when GTK cannot start, for example
/// on a Wayland-only session without XWayland.
#[cfg(target_os = "linux")]
fn linux_gtk_init() -> bool {
    if std::env::var_os("BROWSECRAFT_NO_WEBKIT").is_some() {
        return false;
    }
    // SAFETY: called on the main thread before any other thread is spawned.
    unsafe {
        std::env::set_var("GDK_BACKEND", "x11");
        // WebKitGTK's DMA-BUF renderer cannot allocate GBM buffers under
        // XWayland on hybrid Intel/NVIDIA systems and its X errors abort
        // winit; the shared-memory path is reliable. Overridable.
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        // Run web content in WebKit's bubblewrap sandbox. wry creates the
        // engine's processes before the application can call
        // webkit_web_context_set_sandbox_enabled, so it is forced here.
        if std::env::var_os("BROWSECRAFT_NO_SANDBOX").is_none() {
            std::env::set_var("WEBKIT_FORCE_SANDBOX", "1");
        } else {
            eprintln!("browsecraft: BROWSECRAFT_NO_SANDBOX is set: web content is NOT sandboxed");
        }
    }
    match gtk::init() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("browsecraft: GTK is unavailable ({e}); using the engine for every page");
            false
        }
    }
}
