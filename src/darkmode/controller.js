// BrowseCraft smart dark mode: page-side controller.
//
// The Rust side (`darkmode::page_script`) wraps this file together with the
// vendored Dark Reader engine; the three BC placeholders below (engine,
// config, system colour scheme) are filled in there. The controller decides, for the
// current document, whether dark mode applies, hides the white flash until the
// engine is running, optionally skips pages that are already dark, and
// exposes `window.__bcDark.apply(config, systemDark)` for live updates.
(function () {
  'use strict';
  try {
    if (window.__bcDark) {
      return;
    }
    var proto = location.protocol;
    if (proto !== 'http:' && proto !== 'https:' && proto !== 'file:') {
      return;
    }
    var rootEl = document.documentElement;
    if (!rootEl || rootEl.tagName.toLowerCase() !== 'html') {
      return;
    }

    var engine = null;
    function loadEngine() {
      if (engine) {
        return engine;
      }
      var exports = {};
      var module = { exports: exports };
      var define = undefined;
      __BC_ENGINE__
      engine = exports;
      return engine;
    }

    var cfg = null;
    var systemDark = null; // null: ask matchMedia
    var engineOn = false;
    var nativeDark; // undefined until measured
    var nativeSent = false;
    var tempStyle = null;
    var tempTimer = 0;
    var scheduleTimer = 0;
    var waiting = false;
    var lastReason = '';
    var isTop = true;
    try {
      isTop = window.self === window.top;
    } catch (e) {
      isTop = false;
    }

    // ---- host matching (same rules as `settings::host_matches` in Rust) ----
    function ownHost() {
      var h = '';
      if (!isTop) {
        try {
          h = window.top.location.hostname;
        } catch (e) {
          try {
            var ao = location.ancestorOrigins;
            if (ao && ao.length) {
              h = new URL(ao[ao.length - 1]).hostname;
            }
          } catch (e2) {}
        }
      }
      return normHost(h || location.hostname);
    }
    function normHost(h) {
      h = String(h || '').trim().toLowerCase().replace(/\.$/, '');
      return h.indexOf('www.') === 0 ? h.slice(4) : h;
    }
    function hostMatches(rule, host) {
      rule = normHost(rule);
      if (rule.indexOf('*.') === 0) {
        rule = rule.slice(2);
      }
      return !!rule && !!host && (host === rule || host.slice(-(rule.length + 1)) === '.' + rule);
    }
    function bestSite(host) {
      var best = null;
      var bestLen = -1;
      var sites = cfg.sites || {};
      for (var k in sites) {
        if (Object.prototype.hasOwnProperty.call(sites, k) && hostMatches(k, host) && normHost(k).length > bestLen) {
          best = sites[k];
          bestLen = normHost(k).length;
        }
      }
      return best;
    }
    function isSystemDark() {
      if (systemDark !== null) {
        return systemDark;
      }
      try {
        return matchMedia('(prefers-color-scheme: dark)').matches;
      } catch (e) {
        return false;
      }
    }
    function inSchedule() {
      var s = cfg.schedule;
      if (!s || !s.enabled) {
        return true;
      }
      var d = new Date();
      var m = d.getHours() * 60 + d.getMinutes();
      return s.from <= s.to ? m >= s.from && m < s.to : m >= s.from || m < s.to;
    }

    // Returns {dark, forced, reason}. `forced` means a per-site "on": the
    // native-dark check does not apply.
    function decide(host) {
      if (!cfg || !cfg.enabled) {
        return { dark: false, reason: 'off' };
      }
      var site = bestSite(host);
      if (site && site.mode === 'on') {
        return { dark: true, forced: true, reason: 'site-on' };
      }
      if (site && site.mode === 'off') {
        return { dark: false, reason: 'site-off' };
      }
      var ex = cfg.excluded || [];
      for (var i = 0; i < ex.length; i++) {
        if (hostMatches(ex[i], host)) {
          return { dark: false, reason: 'excluded' };
        }
      }
      var want = cfg.mode === 'dark' || (cfg.mode === 'system' && isSystemDark());
      if (!want) {
        return { dark: false, reason: 'mode-' + cfg.mode };
      }
      if (!inSchedule()) {
        return { dark: false, reason: 'schedule' };
      }
      return { dark: true, forced: false, reason: 'mode-' + cfg.mode };
    }

    // ---- no white flash ----
    function addTemp() {
      if (tempStyle && tempStyle.isConnected) {
        return;
      }
      tempStyle = document.createElement('style');
      tempStyle.id = 'bc-dark-flash';
      tempStyle.textContent =
        'html,body{background-color:#181a1b!important;color:#e8e6e3!important;color-scheme:dark!important}';
      (document.head || rootEl).appendChild(tempStyle);
      clearTimeout(tempTimer);
      tempTimer = setTimeout(removeTemp, 4000);
    }
    function removeTemp() {
      clearTimeout(tempTimer);
      if (tempStyle) {
        tempStyle.remove();
        tempStyle = null;
      }
    }

    // ---- native dark detection ----
    function parseRgb(c) {
      var m = /^rgba?\(([^)]+)\)$/.exec(c || '');
      if (!m) {
        return null;
      }
      var p = m[1].split(/[ ,/]+/).filter(Boolean).map(parseFloat);
      if (p.length < 3) {
        return null;
      }
      return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
    }
    function lum(c) {
      function ch(v) {
        v /= 255;
        return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
      }
      return 0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b);
    }
    function pageIsDark() {
      var cs = '';
      try {
        cs = getComputedStyle(rootEl).colorScheme || '';
      } catch (e) {}
      // `color-scheme: dark` (or `light dark` with a dark system) makes the
      // canvas itself dark even when no element has a background.
      var canvasDark = /\bdark\b/.test(cs) && (!/\blight\b/.test(cs) || isSystemDark());
      // The canvas takes the root's background, else the body's (CSS
      // background propagation), so a short body still paints the viewport.
      var canvasLum = canvasDark ? 0 : 1;
      var holders = [rootEl, document.body];
      for (var k = 0; k < holders.length; k++) {
        if (!holders[k]) {
          continue;
        }
        var hc = parseRgb(getComputedStyle(holders[k]).backgroundColor);
        if (hc && hc.a >= 0.5) {
          canvasLum = lum(hc);
          break;
        }
      }
      var w = window.innerWidth || 800;
      var h = window.innerHeight || 600;
      var pts = [[0.5, 0.5], [0.5, 0.15], [0.25, 0.5], [0.75, 0.5], [0.5, 0.85]];
      var sum = 0;
      var n = 0;
      for (var i = 0; i < pts.length; i++) {
        var stack = document.elementsFromPoint(Math.floor(w * pts[i][0]), Math.floor(h * pts[i][1]));
        var found = null;
        for (var j = 0; j < stack.length; j++) {
          var s = getComputedStyle(stack[j]);
          if (s.backgroundImage && s.backgroundImage !== 'none') {
            continue; // unknown: look at what is behind
          }
          var c = parseRgb(s.backgroundColor);
          if (c && c.a >= 0.5) {
            found = c;
            break;
          }
        }
        sum += found ? lum(found) : canvasLum;
        n++;
      }
      return sum / n < 0.2;
    }
    function measureNative() {
      if (nativeDark !== undefined) {
        return nativeDark;
      }
      // Look at the page as the author made it: our styles are off for the
      // few synchronous instructions of the measurement (no paint between).
      var wasOn = engineOn;
      if (wasOn) {
        stopEngine();
      }
      var hadTemp = !!tempStyle;
      removeTemp();
      try {
        nativeDark = pageIsDark();
      } catch (e) {
        nativeDark = false;
      }
      if (hadTemp && !nativeDark) {
        addTemp();
      }
      if (!nativeSent && isTop) {
        nativeSent = true;
        try {
          window.ipc.postMessage('bc-dark:' + JSON.stringify({ host: ownHost(), nativeDark: nativeDark }));
        } catch (e) {}
      }
      return nativeDark;
    }

    // ---- engine ----
    function theme() {
      return {
        mode: 1,
        brightness: cfg.brightness,
        contrast: cfg.contrast,
        sepia: cfg.sepia,
        grayscale: cfg.grayscale,
        useFont: !!cfg.useFont,
        fontFamily: cfg.fontFamily || '',
        textStroke: cfg.textStroke || 0,
        styleSystemControls: true,
      };
    }
    function startEngine(host) {
      var site = bestSite(host);
      var css = (site && site.css) || '';
      var fixes = css ? { css: css } : null;
      loadEngine().enable(theme(), fixes);
      engineOn = true;
      if (tempStyle) {
        // The engine keeps its own fallback until the style sheets are
        // processed; ours can go once the page has loaded.
        if (document.readyState === 'complete') {
          setTimeout(removeTemp, 150);
        } else {
          window.addEventListener('load', function () { setTimeout(removeTemp, 150); }, { once: true });
        }
      }
    }
    function stopEngine() {
      if (engineOn && engine) {
        try {
          engine.disable();
        } catch (e) {}
      }
      engineOn = false;
    }

    function reconcile() {
      var host = ownHost();
      var d = decide(host);
      lastReason = d.reason;
      if (!d.dark) {
        stopEngine();
        removeTemp();
        return;
      }
      if (!engineOn) {
        addTemp();
      }
      if (cfg.respectNative && !d.forced && nativeDark === undefined && document.readyState === 'loading') {
        // Wait for the first render tree to judge the page.
        if (!waiting) {
          waiting = true;
          document.addEventListener('DOMContentLoaded', function () {
            waiting = false;
            reconcile();
          }, { once: true });
        }
        return;
      }
      if (cfg.respectNative && !d.forced && measureNative()) {
        stopEngine();
        removeTemp();
        lastReason = 'native-dark';
        return;
      }
      startEngine(host);
    }

    function setup(newCfg, sys) {
      cfg = newCfg;
      if (sys === true || sys === false) {
        systemDark = sys;
      }
      clearInterval(scheduleTimer);
      scheduleTimer = 0;
      if (cfg.schedule && cfg.schedule.enabled) {
        scheduleTimer = setInterval(reconcile, 60000);
      }
      reconcile();
    }

    try {
      matchMedia('(prefers-color-scheme: dark)').addEventListener('change', function () {
        if (systemDark === null) {
          reconcile();
        }
      });
    } catch (e) {}

    var api = {
      apply: setup,
      state: function () {
        return { engine: engineOn, host: ownHost(), reason: lastReason, nativeDark: nativeDark };
      },
    };
    Object.defineProperty(window, '__bcDark', { value: api, enumerable: false, configurable: true });
    setup(__BC_CONFIG__, __BC_SYSTEM_DARK__);
  } catch (e) {
    // Dark mode must never break a page.
  }
})();
