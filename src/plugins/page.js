// BrowseCraft plugin runtime. Generated data replaces __BC_DATA__; the rest is
// static. Each plugin file runs in its own function scope with its own frozen
// `bc` object. This all happens in the PAGE world (WebKit user scripts have
// no separate world here), so a hostile page can still tamper with built-ins
// before or after us; see docs/EXTENSIONS.md "Isolation limits".
(function (D) {
  'use strict';
  if (window.__bcDeliver) return;
  var NONCE = D.nonce, isTop = window === window.top, L = location;
  var post = null;
  try { post = window.ipc.postMessage.bind(window.ipc); } catch (e) {}
  var MAX = 60000;

  // ---- match patterns (compiled by Rust: [scheme, hostMode, host, path]) ----
  function esc(s) { return s.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[\\s\\S]*'); }
  function one(pt) {
    var sc = L.protocol.slice(0, -1), s = pt[0];
    if (s === '*web') { if (sc !== 'http' && sc !== 'https') return false; }
    else if (s === '<all>') { if (['http', 'https', 'file', 'ftp'].indexOf(sc) < 0) return false; }
    else if (s !== sc) return false;
    var h = L.hostname.toLowerCase(), n = pt[2];
    if (pt[1] === 1) { if (h !== n && !(h.length > n.length && h.slice(-n.length - 1) === '.' + n)) return false; }
    else if (pt[1] === 2 && h !== n) return false;
    if (!pt.rx) pt.rx = new RegExp('^' + esc(pt[3]) + '$');
    return pt.rx.test(L.pathname + L.search);
  }
  function any(list) { for (var i = 0; i < list.length; i++) if (one(list[i])) return true; return false; }

  // ---- bridge ----
  var waiting = Object.create(null);
  function send(pid, id, m, a) {
    var body = JSON.stringify({ n: NONCE, p: pid, i: id, m: m, a: a === undefined ? null : a });
    if (body.length > MAX) throw new Error('message too large');
    post('bc-plugin:' + body);
  }
  Object.defineProperty(window, '__bcDeliver', {
    value: function (pid, id, ok, json) {
      var w = waiting[pid + ':' + id];
      if (!w) return;
      delete waiting[pid + ':' + id];
      var v;
      try { v = JSON.parse(json); } catch (e) { v = null; }
      if (ok) w[0](v); else w[1](new Error(String(v)));
    }
  });

  function makeBc(P) {
    var seq = 0;
    function call(m, a) {
      return new Promise(function (res, rej) {
        if (!post) return rej(new Error('bridge unavailable'));
        // Replies are delivered to the top frame only.
        if (!isTop) return rej(new Error('bc calls are only answered in the top frame'));
        var id = ++seq;
        waiting[P.id + ':' + id] = [res, rej];
        try { send(P.id, id, m, a); } catch (e) { delete waiting[P.id + ':' + id]; rej(e); }
      });
    }
    var bc = {
      id: P.id, version: P.v, apiVersion: P.api,
      settings: Object.freeze(JSON.parse(JSON.stringify(P.settings))),
      permissions: Object.freeze(P.perms.slice()),
      log: function () {
        try { send(P.id, 0, 'log', Array.prototype.map.call(arguments, String).join(' ').slice(0, 2000)); } catch (e) {}
      },
      sendMessage: function (obj) { return call('message', obj); },
      notify: function (title, body) { return call('notify', { title: String(title), body: String(body === undefined ? '' : body) }); },
      storage: Object.freeze({
        get: function (k) { return call('storage.get', String(k)); },
        set: function (k, v) { return call('storage.set', { key: String(k), value: v }); },
        remove: function (k) { return call('storage.remove', String(k)); }
      }),
      clipboard: Object.freeze({ writeText: function (t) { return call('clipboard.write', String(t)); } }),
      tabs: Object.freeze({
        query: function () { return call('tabs.query', null); },
        open: function (u) { return call('tabs.open', String(u)); }
      })
    };
    return Object.freeze(bc);
  }

  // ---- injection ----
  function report(P, what, e) {
    try { send(P.id, 0, 'log', 'error in ' + what + ': ' + (e && e.stack || e)); } catch (x) {}
  }
  function addCss(P, css) {
    try {
      if (window.CSSStyleSheet && 'adoptedStyleSheets' in document) {
        // Constructable sheets are not subject to the page's inline-style CSP.
        var sh = new CSSStyleSheet();
        sh.replaceSync(css);
        document.adoptedStyleSheets = Array.prototype.concat.call(document.adoptedStyleSheets, [sh]);
        return;
      }
    } catch (e) {}
    var put = function () {
      var st = document.createElement('style');
      st.setAttribute('data-bc-plugin', P.id);
      st.textContent = css;
      (document.head || document.documentElement).appendChild(st);
    };
    if (document.documentElement) put(); else document.addEventListener('DOMContentLoaded', put, { once: true });
  }
  function runJs(P, bc, name, code) {
    var f;
    try {
      f = new Function('bc', '"use strict";\n' + code + '\n//# sourceURL=bc-plugin://' + P.id + '/' + name);
    } catch (e) { return report(P, name + ' (could not compile; a page CSP may forbid it)', e); }
    try { f.call(undefined, bc); } catch (e) { report(P, name, e); }
  }
  function when(at, fn) {
    var rs = document.readyState;
    if (at === 'document_start') return fn();
    if (at === 'document_end') {
      if (rs !== 'loading') fn(); else document.addEventListener('DOMContentLoaded', fn, { once: true });
      return;
    }
    var go = function () { (window.requestIdleCallback ? window.requestIdleCallback(fn, { timeout: 2000 }) : setTimeout(fn, 50)); };
    if (rs === 'complete') go(); else window.addEventListener('load', go, { once: true });
  }

  D.plugins.forEach(function (P) {
    var bc = null;
    P.scripts.forEach(function (S) {
      if (!isTop && !S.frames) return;
      if (!any(S.inc) || any(S.exc)) return;
      when(S.at, function () {
        S.css.forEach(function (c) { addCss(P, c[1]); });
        if (S.js.length && !bc) bc = makeBc(P);
        S.js.forEach(function (j) { runJs(P, bc, j[0], j[1]); });
      });
    });
  });
})(__BC_DATA__);
