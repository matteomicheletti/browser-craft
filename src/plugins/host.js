// BrowseCraft plugin page runtime (API 1.1): `bc.host` for pages served
// from bc-plugin://<id>/. Injected into every web view at document start,
// main frame only; it does nothing anywhere else. The browser checks the
// sender's address and this nonce on every call, so a web page gets nothing
// from it even if it were somehow defined there.
(function () {
  "use strict";
  if (location.protocol !== "bc-plugin:" || window.top !== window) return;
  if (window.__bcHostDeliver) return;
  const DATA = __BC_HOST__;
  const ipc = window.ipc;
  if (!ipc || typeof ipc.postMessage !== "function") return;
  const post = ipc.postMessage.bind(ipc);
  const pending = new Map();
  const listeners = new Map();
  let next = 1;

  function call(method, args) {
    return new Promise(function (resolve, reject) {
      const id = next++;
      pending.set(id, { resolve: resolve, reject: reject });
      try {
        post("bc-host:" + JSON.stringify({ n: DATA.nonce, i: id, m: method, a: args === undefined ? null : args }));
      } catch (e) {
        pending.delete(id);
        reject(e);
      }
    });
  }

  function emit(name, detail) {
    const set = listeners.get(name);
    if (!set) return;
    for (const fn of Array.from(set)) {
      try { fn(detail); } catch (e) { console.error("bc.host listener", name, e); }
    }
  }

  const define = function (name, value) {
    Object.defineProperty(window, name, { value: value, configurable: false, enumerable: false, writable: false });
  };
  define("__bcHostDeliver", function (id, ok, json) {
    const p = pending.get(id);
    if (!p) return;
    pending.delete(id);
    let v = null;
    try { v = JSON.parse(json); } catch (e) { v = json; }
    if (ok) p.resolve(v); else p.reject(new Error(String(v)));
  });
  define("__bcHostEvent", function (name, json) {
    let v = null;
    try { v = JSON.parse(json); } catch (e) { v = json; }
    emit(String(name), v);
  });

  // Theme: CSS variables on :root, replaced live when the browser theme changes.
  let style = null;
  function applyTheme(css, dark) {
    if (!style) {
      style = document.createElement("style");
      style.id = "bc-theme";
    }
    style.textContent = css;
    const root = document.documentElement;
    if (root) {
      (document.head || root).appendChild(style);
      root.dataset.bcTheme = dark ? "dark" : "light";
    }
  }
  define("__bcHostTheme", function (css, dark, name) {
    DATA.theme = { css: css, dark: !!dark, name: String(name || "") };
    applyTheme(css, dark);
    emit("theme", { dark: !!dark, name: String(name || "") });
  });
  if (document.documentElement) applyTheme(DATA.theme.css, DATA.theme.dark);
  document.addEventListener("DOMContentLoaded", function () { applyTheme(DATA.theme.css, DATA.theme.dark); });

  // Esc closes a modal unless the page handled the key itself.
  window.addEventListener("keydown", function (e) {
    if (e.key === "Escape" && !e.defaultPrevented && DATA.modal) {
      e.preventDefault();
      call("page.close").catch(function () {});
    }
  });

  const host = Object.freeze({
    apiVersion: DATA.api,
    plugin: location.hostname,
    modal: !!DATA.modal,
    call: call,
    on: function (name, fn) {
      if (typeof fn !== "function") return function () {};
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(fn);
      return function () { listeners.get(name).delete(fn); };
    },
    info: function () { return call("page.info"); },
    close: function () { return call("page.close"); },
    notify: function (title, body) { return call("notify", { title: String(title), body: String(body || "") }); },
    settings: Object.freeze({
      get: function () { return call("settings.get"); },
      set: function (key, value) { return call("settings.set", { key: String(key), value: value }); },
    }),
    clipboard: Object.freeze({
      list: function (query, limit) { return call("clipboard.list", { query: String(query || ""), limit: limit || 200 }); },
      get: function (id) { return call("clipboard.get", { id: id }); },
      paste: function (id, opts) { return call("clipboard.paste", { id: id, plain: !!(opts && opts.plain) }); },
      copy: function (id) { return call("clipboard.copy", { id: id }); },
      remove: function (id) { return call("clipboard.delete", { id: id }); },
      pin: function (id, pinned) { return call("clipboard.pin", { id: id, pinned: !!pinned }); },
      clear: function (opts) { return call("clipboard.clear", { keep_pinned: !(opts && opts.pinnedToo) }); },
      usage: function () { return call("clipboard.usage"); },
    }),
    screenshot: Object.freeze({
      get: function () { return call("screenshot.get"); },
      copyImage: function (dataUrl) { return call("screenshot.copy", { data: String(dataUrl) }); },
      saveFile: function (name, dataUrl, opts) {
        return call("screenshot.save", { name: String(name || ""), data: String(dataUrl), ask: !!(opts && opts.ask) });
      },
    }),
  });
  define("bc", Object.freeze({ host: host }));
})();
