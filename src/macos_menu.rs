//! macOS: the application's Edit menu.
//!
//! AppKit sends Cmd+A, Cmd+C, Cmd+V, Cmd+X and Cmd+Z to the focused view
//! through the menu bar's key equivalents: without an Edit menu, WKWebView
//! receives the keys (the page sees `keydown`) but never selects, copies or
//! pastes. winit only builds the application menu, so this adds the Edit menu
//! next to it once the event loop runs.
//!
//! A plugin may take Cmd+V (the clipboard manager): its Paste item then
//! loses the key equivalent, and the plugin pastes with `paste` afterwards.
//!
//! The browser's own Cmd shortcuts (Cmd+T, Cmd+L, Cmd+P...) and the plugins'
//! do not come back from WKWebView to the window, so a local event monitor
//! (`install_key_monitor`) takes them while a page has the keyboard, before
//! WKWebView and the menu see them.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadMarker, MainThreadOnly as _, sel};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSMenu, NSMenuItem, NSView, NSWindow};
use objc2_foundation::NSString;
use winit::event_loop::EventLoopProxy;

use crate::browser::UserMessage;

thread_local! {
    /// The Paste and Paste and Match Style items, to give or take their keys.
    static PASTE: RefCell<Option<(Retained<NSMenuItem>, Retained<NSMenuItem>)>> = const { RefCell::new(None) };
    /// Whether a plugin has Cmd+V / Cmd+Shift+V (set before `install`).
    static TAKEN: RefCell<(bool, bool)> = const { RefCell::new((false, false)) };
}

/// Adds the Edit menu (once; main thread only).
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if PASTE.with(|p| p.borrow().is_some()) {
        return;
    }
    let app = NSApplication::sharedApplication(mtm);
    let main = app.mainMenu().unwrap_or_else(|| {
        let menu = NSMenu::new(mtm);
        app.setMainMenu(Some(&menu));
        menu
    });
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edit"));
    let cmd = NSEventModifierFlags::Command;
    let add = |title: &str, action: Sel, key: &str, mods: NSEventModifierFlags| {
        // SAFETY: the selectors are standard NSResponder actions, sent to the
        // first responder (no target), which ignores those it does not know.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), &NSString::from_str(key))
        };
        item.setKeyEquivalentModifierMask(mods);
        edit.addItem(&item);
        item
    };
    add("Undo", sel!(undo:), "z", cmd);
    add("Redo", sel!(redo:), "z", cmd | NSEventModifierFlags::Shift);
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    add("Cut", sel!(cut:), "x", cmd);
    add("Copy", sel!(copy:), "c", cmd);
    let paste = add("Paste", sel!(paste:), "v", cmd);
    let plain = add("Paste and Match Style", sel!(pasteAsPlainText:), "v", cmd | NSEventModifierFlags::Shift);
    add("Select All", sel!(selectAll:), "a", cmd);
    let item = NSMenuItem::new(mtm);
    item.setSubmenu(Some(&edit));
    main.addItem(&item);
    PASTE.with(|p| *p.borrow_mut() = Some((paste, plain)));
    let (v, shift_v) = TAKEN.with(|t| *t.borrow());
    apply(v, shift_v);
}

/// A plugin took (or gave back) Cmd+V and Cmd+Shift+V: the Paste items keep
/// their keys only while no plugin has them.
pub fn plugin_paste_keys(v: bool, shift_v: bool) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    TAKEN.with(|t| *t.borrow_mut() = (v, shift_v));
    apply(v, shift_v);
}

fn apply(v: bool, shift_v: bool) {
    PASTE.with(|p| {
        if let Some((paste, plain)) = &*p.borrow() {
            paste.setKeyEquivalent(&NSString::from_str(if v { "" } else { "v" }));
            plain.setKeyEquivalent(&NSString::from_str(if shift_v { "" } else { "v" }));
        }
    });
}

/// Sends `paste:` (or `pasteAsPlainText:`) to the focused view, as the menu
/// item would.
pub fn paste(plain: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let action = if plain { sel!(pasteAsPlainText:) } else { sel!(paste:) };
    // SAFETY: a standard NSResponder action with no target and no sender.
    unsafe {
        app.sendAction_to_from(action, None, None);
    }
}

/// Starts the key monitor (once; main thread only): a Cmd key typed while a
/// web view has the keyboard goes to the browser when it is one of its
/// shortcuts or a plugin's, and on to the page otherwise.
pub fn install_key_monitor(proxy: EventLoopProxy<UserMessage>) {
    thread_local! {
        static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    if INSTALLED.with(|i| i.replace(true)) {
        return;
    }
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a valid event for the call.
        let ev = unsafe { event.as_ref() };
        match page_key(ev, mtm) {
            Some(message) => {
                if !ev.isARepeat() {
                    let _ = proxy.send_event(message);
                }
                std::ptr::null_mut()
            }
            None => event.as_ptr(),
        }
    });
    // SAFETY: the block returns the event it was given or null.
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) };
    // The monitor lives as long as the application.
    std::mem::forget(monitor);
}

fn page_key(ev: &NSEvent, mtm: MainThreadMarker) -> Option<UserMessage> {
    let flags = ev.modifierFlags();
    if !flags.contains(NSEventModifierFlags::Command) {
        return None;
    }
    let window = ev.window(mtm)?;
    if !web_view_has_keyboard(&window) {
        return None;
    }
    if flags.contains(NSEventModifierFlags::Control) {
        // Ctrl+Cmd+F, the system's full screen key; other Control chords
        // stay with the page.
        let f = ev.charactersIgnoringModifiers().is_some_and(|c| c.to_string().eq_ignore_ascii_case("f"));
        return f.then(crate::browser::mac_fullscreen_key);
    }
    let key = match ev.keyCode() {
        48 => "tab".to_string(),
        116 => "pageup".to_string(),
        121 => "pagedown".to_string(),
        123 => "left".to_string(),
        124 => "right".to_string(),
        117 => "delete".to_string(),
        _ => ev.charactersIgnoringModifiers()?.to_string().to_lowercase(),
    };
    if key.is_empty() {
        return None;
    }
    crate::browser::mac_page_key(&key, flags.contains(NSEventModifierFlags::Shift), flags.contains(NSEventModifierFlags::Option))
}

/// Whether the window's first responder is (inside) a WKWebView, not the
/// browser's own view (the chrome handles its keys through winit).
fn web_view_has_keyboard(window: &NSWindow) -> bool {
    let Some(responder) = window.firstResponder() else { return false };
    let mut view = responder.downcast::<NSView>().ok();
    while let Some(v) = view {
        // wry's web view is a subclass (`WryWebView`): look up the classes.
        let mut class = Some(v.class());
        while let Some(c) = class {
            if c.name().to_bytes().starts_with(b"WK") {
                return true;
            }
            class = c.superclass();
        }
        // SAFETY: reading a view's superview on the main thread.
        view = unsafe { v.superview() };
    }
    false
}
