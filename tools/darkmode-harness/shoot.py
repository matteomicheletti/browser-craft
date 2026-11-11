#!/usr/bin/env python3
"""Loads a page in WebKitGTK with a dark mode init script and saves a PNG.

usage: shoot.py SCRIPT.js PAGE.html OUT.png [WAIT_MS] [UPDATE.js]

UPDATE.js (optional) is evaluated after the page loaded, like the browser's
live `update_script`; the screenshot is taken WAIT_MS after that.

SCRIPT.js comes from `cargo test --bin browsecraft dump_scripts -- --ignored`
(set BC_DARK_DUMP_DIR). Needs python3-gobject with WebKit2 4.1 and a display.
It mimics wry: the script runs at document start in the page world, and
`window.ipc.postMessage` reaches a "ipc" script message handler.
Prints the page's `__bcDark.state()` and the IPC messages as JSON.
"""
import json
import os
import sys

# Offscreen windows have no GL; the software path is enough for screenshots.
os.environ.setdefault("WEBKIT_DISABLE_COMPOSITING_MODE", "1")
os.environ.setdefault("WEBKIT_DISABLE_DMABUF_RENDERER", "1")

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import GLib, Gtk, WebKit2  # noqa: E402

script_path, page, out = sys.argv[1:4]
wait_ms = int(sys.argv[4]) if len(sys.argv) > 4 else 1500
update_js = open(sys.argv[5]).read() if len(sys.argv) > 5 else None

ucm = WebKit2.UserContentManager()
messages = []
ucm.register_script_message_handler("ipc")
ucm.connect("script-message-received::ipc", lambda m, r: messages.append(r.get_js_value().to_string()))
shim = "window.ipc={postMessage:function(m){window.webkit.messageHandlers.ipc.postMessage(m)}};"
body = shim + (open(script_path).read() if script_path != "none" else "")
# Records whether the no-flash style exists while the document still loads.
body += ";window.__flashAtStart=document.readyState+':'+!!document.getElementById('bc-dark-flash');"
ucm.add_script(WebKit2.UserScript(
    body, WebKit2.UserContentInjectedFrames.ALL_FRAMES, WebKit2.UserScriptInjectionTime.START, None, None))

view = WebKit2.WebView.new_with_user_content_manager(ucm)
view.get_settings().set_enable_developer_extras(False)
win = Gtk.OffscreenWindow()
win.set_default_size(900, 720)
win.add(view)
win.show_all()


def done(_view, res):
    surface = view.get_snapshot_finish(res)
    surface.write_to_png(out)
    view.evaluate_javascript(
        "JSON.stringify({s:window.__bcDark?window.__bcDark.state():null,flash:window.__flashAtStart,flashNow:!!document.getElementById('bc-dark-flash')})", -1, None, None, None, report, None)


def report(_view, res, _data):
    try:
        state = view.evaluate_javascript_finish(res).to_string()
    except Exception as e:  # noqa: BLE001
        state = repr(e)
    print(json.dumps({"state": state, "ipc": messages, "png": out}))
    Gtk.main_quit()


def snap():
    view.get_snapshot(WebKit2.SnapshotRegion.VISIBLE, WebKit2.SnapshotOptions.NONE, None, done)
    return False


def loaded(_view, event):
    if event == WebKit2.LoadEvent.FINISHED:
        if update_js:
            view.evaluate_javascript(update_js, -1, None, None, None, None, None)
        GLib.timeout_add(wait_ms, snap)


view.connect("load-changed", loaded)
GLib.timeout_add(20000, lambda: (print("timeout"), Gtk.main_quit()))
view.load_uri("file://" + page if page.startswith("/") else page)
Gtk.main()
