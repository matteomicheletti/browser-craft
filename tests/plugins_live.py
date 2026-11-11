#!/usr/bin/env python3
"""Live check of the plugin page script in a real WebKitGTK 4.1.

Driven by the ignored Rust test `plugins::script::live::plugin_script_in_webkit`
(`cargo test plugins::script::live -- --ignored --nocapture`):

  plugins_live.py SCRIPT_JS OUT_DIR HOST:PATH...

Stdout protocol (one JSON per line): `{"msg": "<ipc body>"}` for every
`window.ipc.postMessage`, answered by the Rust side with one stdin line
`{"reply": "<script>" | null}`; `{"result": {...}}` once per page.
Needs python3-gi with WebKit2 4.1; renders offscreen and writes PNGs.
"""
import http.server, json, sys, threading
import gi
gi.require_version('Gtk', '3.0')
gi.require_version('WebKit2', '4.1')
from gi.repository import GLib, Gtk, WebKit2

script_path, out_dir, targets = sys.argv[1], sys.argv[2], sys.argv[3:]

PAGE = """<!doctype html><meta charset=utf-8><title>Live</title>
<style>body{font:16px sans-serif;margin:24px;background:#fff;color:#222}</style>
<h1>Plugin live test</h1><div class="mw-body-content" id="mw-content-text"><p>%s</p></div>
<iframe src="/frame" width="300" height="60"></iframe>"""
WORDS = " ".join(["word"] * 60)


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        body = (PAGE % WORDS).encode() if self.path != "/frame" else b"<p id=f>frame</p>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        if "csp" in self.path:
            self.send_header("Content-Security-Policy", "default-src 'none'; style-src 'none'; script-src 'none'")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
port = srv.server_address[1]
threading.Thread(target=srv.serve_forever, daemon=True).start()

Gtk.init([])
ucm = WebKit2.UserContentManager()
ucm.register_script_message_handler("ipc")


def on_msg(_m, value):
    # Blocks the UI thread briefly: the Rust side answers immediately.
    print(json.dumps({"msg": value.get_js_value().to_string()}), flush=True)
    line = sys.stdin.readline()
    reply = json.loads(line).get("reply") if line else None
    if reply:
        GLib.idle_add(lambda: view.evaluate_javascript(reply, -1, None, None, None, None, None) and False)


ucm.connect("script-message-received::ipc", on_msg)
shim = WebKit2.UserScript(
    "window.ipc={postMessage:function(m){window.webkit.messageHandlers.ipc.postMessage(m)}};",
    WebKit2.UserContentInjectedFrames.ALL_FRAMES, WebKit2.UserScriptInjectionTime.START, None, None)
ucm.add_script(shim)
ucm.add_script(WebKit2.UserScript(open(script_path).read(), WebKit2.UserContentInjectedFrames.ALL_FRAMES,
                                  WebKit2.UserScriptInjectionTime.START, None, None))
view = WebKit2.WebView.new_with_user_content_manager(ucm)
win = Gtk.OffscreenWindow()
win.set_default_size(900, 420)
win.add(view)
win.show_all()

FACTS = """JSON.stringify({
  banner: (document.getElementById('bc-hello-banner') || {}).textContent || null,
  badge: (document.querySelector('.bc-word-badge') || {}).textContent || null,
  lineHeight: (document.querySelector('.mw-body-content') ? getComputedStyle(document.querySelector('.mw-body-content')).lineHeight : null),
  bannerBg: (document.getElementById('bc-hello-banner') ? getComputedStyle(document.getElementById('bc-hello-banner')).backgroundColor : null),
  deliver: typeof window.__bcDeliver,
  leaked: typeof window.bc
})"""
state = {"i": 0}


def step():
    if state["i"] >= len(targets):
        Gtk.main_quit()
        return False
    host, path = targets[state["i"]].split(":", 1)
    view.load_uri("http://%s:%d%s" % (host, port, path))
    return False


def on_load(_v, event):
    if event != WebKit2.LoadEvent.FINISHED:
        return
    GLib.timeout_add(1500, collect)


def collect():
    def done(v, res, _d):
        facts = json.loads(v.evaluate_javascript_finish(res).to_string())
        n = state["i"]

        def shot(vw, r, _d2):
            surf = vw.get_snapshot_finish(r)
            surf.write_to_png("%s/page%d.png" % (out_dir, n))
            print(json.dumps({"result": {"target": targets[n], "facts": facts}}), flush=True)
            state["i"] += 1
            GLib.idle_add(step)

        view.get_snapshot(WebKit2.SnapshotRegion.VISIBLE, WebKit2.SnapshotOptions.NONE, None, shot, None)

    view.evaluate_javascript(FACTS, -1, None, None, None, done, None)
    return False


view.connect("load-changed", on_load)
GLib.idle_add(step)
GLib.timeout_add(60000, lambda: (print(json.dumps({"error": "timeout"}), flush=True), Gtk.main_quit()))
Gtk.main()
