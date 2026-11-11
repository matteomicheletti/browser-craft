//! The JavaScript the `page.*` methods run in the page, and a small driver
//! (`PageJob`) that turns a request into scripts and their results into the
//! reply. The browser runs each script with wry's
//! `evaluate_script_with_callback` and feeds the callback's string back to
//! `PageJob::on_result`; the headless mode does the same with WebKit's
//! `run_javascript`. Both return the script's completion value as JSON, and
//! an empty string when the script could not run at all.
//!
//! Every value from a request reaches the script as a JSON literal
//! (`js_string`, `serde_json`), never spliced in as code. Scripts never throw:
//! they return `{ok:true,result}`, `{ok:true,pending:id}` (a promise, polled
//! with `eval_poll_script`) or `{ok:false,code,error}`.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::{
    self, ClickParams, EvalParams, Method, PressParams, ReadParams, RpcError, ScrollParams,
    ScrollTo, SnapshotParams, Target, TypeParams, WaitParams, parse_params,
};

/// How often `page.wait` and pending `page.eval` promises are polled.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// `s` as a JavaScript string literal. JSON strings are JavaScript strings;
/// U+2028/U+2029 are escaped too for engines older than ES2019.
pub fn js_string(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn js_opt(s: Option<&str>) -> String {
    s.map_or_else(|| "null".into(), js_string)
}

/// The target as `(selector, ref)` literals.
fn js_target(target: Option<&Target>) -> (String, String) {
    match target {
        Some(Target::Selector(s)) => (js_string(s), "null".into()),
        Some(Target::Ref(r)) => ("null".into(), js_string(r)),
        None => ("null".into(), "null".into()),
    }
}

/// Helpers shared by the scripts: deep (shadow DOM) lookup, element
/// description, visibility.
const HELPERS: &str = r#"
  function deepQuery(root, sel) {
    var found = root.querySelector(sel);
    if (found) return found;
    var all = root.querySelectorAll('*');
    for (var i = 0; i < all.length; i++) {
      if (all[i].shadowRoot) { found = deepQuery(all[i].shadowRoot, sel); if (found) return found; }
    }
    return null;
  }
  function find(sel, ref) {
    try {
      if (ref) return deepQuery(document, '[data-bc-ref="' + ref + '"]');
      if (sel) return deepQuery(document, sel);
    } catch (e) { throw { code: 'invalid_selector', message: 'Invalid selector: ' + sel }; }
    return null;
  }
  function clip(s, n) { s = String(s == null ? '' : s).replace(/\s+/g, ' ').trim(); return s.length > n ? s.slice(0, n - 1) + '…' : s; }
  function nameOf(el) {
    var t;
    if ((t = el.getAttribute('aria-label'))) return clip(t, 120);
    if ((t = el.getAttribute('aria-labelledby'))) {
      var parts = t.split(/\s+/).map(function (id) { var l = document.getElementById(id); return l ? l.innerText : ''; });
      if (parts.join('').trim()) return clip(parts.join(' '), 120);
    }
    if (el.labels && el.labels.length) return clip(el.labels[0].innerText, 120);
    if (el.tagName === 'INPUT' && /^(submit|button|reset)$/i.test(el.type) && el.value) return clip(el.value, 120);
    if ((t = el.getAttribute('alt'))) return clip(t, 120);
    var text = el.tagName === 'SELECT' ? '' : (el.innerText || el.textContent);
    if (text && text.trim()) return clip(text, 120);
    var img = el.querySelector && el.querySelector('img[alt]');
    if (img && img.alt) return clip(img.alt, 120);
    if ((t = el.getAttribute('title'))) return clip(t, 120);
    if ((t = el.getAttribute('placeholder'))) return clip(t, 120);
    if ((t = el.getAttribute('name'))) return clip(t, 120);
    return '';
  }
  function roleOf(el) {
    var r = el.getAttribute('role');
    if (r) return r;
    var tag = el.tagName.toLowerCase();
    if (tag === 'a') return 'link';
    if (tag === 'button' || tag === 'summary') return 'button';
    if (tag === 'select') return el.multiple ? 'listbox' : 'combobox';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'input') {
      var t = (el.type || 'text').toLowerCase();
      if (/^(button|submit|reset|image)$/.test(t)) return 'button';
      if (t === 'checkbox' || t === 'radio') return t;
      if (t === 'range') return 'slider';
      if (t === 'search') return 'searchbox';
      if (t === 'number') return 'spinbutton';
      return 'textbox';
    }
    if (el.isContentEditable) return 'textbox';
    return 'generic';
  }
  function describe(el) {
    var n = nameOf(el);
    return roleOf(el) + (n ? ' "' + n + '"' : ' <' + el.tagName.toLowerCase() + '>');
  }
  function visible(el) {
    if (!el.getClientRects().length) return false;
    var s = getComputedStyle(el);
    return s.visibility !== 'hidden' && s.display !== 'none' && parseFloat(s.opacity || '1') > 0;
  }
  function fail(code, error) { return { ok: false, code: code, error: error }; }
  function missing(sel, ref) { return fail('no_element', 'No element matches ' + (ref ? 'ref ' + ref + ' (take a new page.snapshot)' : 'selector ' + sel)); }
"#;

/// Wraps a body in an IIFE with the helpers, turning thrown helper errors
/// into `{ok:false}`.
fn wrap(body: &str) -> String {
    format!(
        "(function () {{ 'use strict';{HELPERS}\n  try {{\n{body}\n  }} catch (e) {{ return fail(e && e.code || 'script', String(e && e.message || e)); }}\n}})()"
    )
}

/// `page.text`: the visible text of the document or of one element.
pub fn text_script(selector: Option<&str>, max_chars: usize) -> String {
    wrap(&format!(
        r#"    var sel = {sel}, max = {max_chars};
    var root = sel ? find(sel, null) : (document.body || document.documentElement);
    if (!root) return sel ? missing(sel, null) : {{ ok: true, result: {{ url: location.href, title: document.title, text: '', truncated: false }} }};
    var text = String(root.innerText || root.textContent || '').replace(/[ \t]+\n/g, '\n').replace(/\n{{3,}}/g, '\n\n').trim();
    var truncated = text.length > max;
    return {{ ok: true, result: {{ url: location.href, title: document.title, text: truncated ? text.slice(0, max) : text, truncated: truncated }} }};"#,
        sel = js_opt(selector)
    ))
}

/// `page.html`: `outerHTML` of the document or of one element.
pub fn html_script(selector: Option<&str>, max_chars: usize) -> String {
    wrap(&format!(
        r#"    var sel = {sel}, max = {max_chars};
    var root = sel ? find(sel, null) : document.documentElement;
    if (!root) return missing(sel, null);
    var html = root.outerHTML || '';
    var truncated = html.length > max;
    return {{ ok: true, result: {{ url: location.href, html: truncated ? html.slice(0, max) : html, truncated: truncated }} }};"#,
        sel = js_opt(selector)
    ))
}

/// `page.snapshot`: the visible interactive elements with stable refs. Refs
/// are stored as `data-bc-ref` attributes, so they survive later snapshots
/// and can be used as selectors. Password values are never included.
pub fn snapshot_script(max_elements: usize, viewport_only: bool) -> String {
    wrap(&format!(
        r#"    var max = {max_elements}, viewportOnly = {viewport_only};
    var SEL = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[contenteditable=""],[contenteditable=true],[onclick],[tabindex]:not([tabindex="-1"]),' +
      '[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=menuitemcheckbox],[role=option],[role=switch],[role=textbox],[role=combobox],[role=searchbox],[role=slider],[role=treeitem]';
    var seq = window.__bcRefSeq || 0, out = [], seen = new Set(), truncated = false;
    var vw = window.innerWidth, vh = window.innerHeight;
    function collect(root) {{
      var list = root.querySelectorAll(SEL);
      for (var i = 0; i < list.length; i++) {{
        var el = list[i];
        if (seen.has(el) || !visible(el)) continue;
        seen.add(el);
        var rect = el.getBoundingClientRect();
        var inView = rect.bottom > 0 && rect.right > 0 && rect.top < vh && rect.left < vw;
        if (viewportOnly && !inView) continue;
        if (out.length >= max) {{ truncated = true; return; }}
        var ref = el.getAttribute('data-bc-ref');
        if (!ref || !/^e\d{{1,9}}$/.test(ref)) {{ ref = 'e' + (++seq); el.setAttribute('data-bc-ref', ref); }}
        var tag = el.tagName.toLowerCase(), role = roleOf(el);
        var item = {{ ref: ref, role: role, name: nameOf(el), tag: tag }};
        var type = (el.type || '').toLowerCase();
        if (tag === 'a' && el.href) item.href = clip(el.href, 300);
        if ((tag === 'input' || tag === 'textarea' || tag === 'select') && type !== 'password' && !/^(button|submit|reset|image|checkbox|radio)$/.test(type) && el.value) item.value = clip(el.value, 200);
        if (type === 'checkbox' || type === 'radio') item.checked = !!el.checked;
        else if (el.hasAttribute('aria-checked')) item.checked = el.getAttribute('aria-checked') === 'true';
        if (el.disabled || el.getAttribute('aria-disabled') === 'true') item.disabled = true;
        if (inView) item.in_viewport = true;
        out.push(item);
      }}
      var all = root.querySelectorAll('*');
      for (var j = 0; j < all.length && !truncated; j++) if (all[j].shadowRoot) collect(all[j].shadowRoot);
    }}
    collect(document);
    window.__bcRefSeq = seq;
    var headings = [];
    document.querySelectorAll('h1,h2,h3').forEach(function (h) {{ if (headings.length < 40 && visible(h)) {{ var t = clip(h.innerText, 120); if (t) headings.push(h.tagName.toLowerCase() + ' ' + t); }} }});
    return {{ ok: true, result: {{ url: location.href, title: document.title, headings: headings, elements: out, truncated: truncated }} }};"#
    ))
}

/// `page.click`: scrolls the element into view, sends pointer and mouse
/// events at its centre, focuses it and clicks.
pub fn click_script(target: &Target) -> String {
    let (sel, r) = js_target(Some(target));
    wrap(&format!(
        r#"    var sel = {sel}, ref = {r};
    var el = find(sel, ref);
    if (!el) return missing(sel, ref);
    if (el.disabled) return fail('disabled', describe(el) + ' is disabled');
    el.scrollIntoView({{ block: 'center', inline: 'center' }});
    var rect = el.getBoundingClientRect();
    var o = {{ bubbles: true, cancelable: true, composed: true, view: window, button: 0, buttons: 1,
      clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 }};
    var P = window.PointerEvent || MouseEvent;
    el.dispatchEvent(new P('pointerdown', o)); el.dispatchEvent(new MouseEvent('mousedown', o));
    if (el.focus) el.focus({{ preventScroll: true }});
    o.buttons = 0;
    el.dispatchEvent(new P('pointerup', o)); el.dispatchEvent(new MouseEvent('mouseup', o));
    var what = describe(el);
    el.click();
    return {{ ok: true, result: {{ ok: true, description: what }} }};"#
    ))
}

/// `page.type`: sets the value through the native setter (so frameworks
/// that track it, such as React, see the change), then fires `input` and
/// `change`; content-editable elements get `insertText`. A `<select>`
/// chooses the option whose value or label is `text`.
pub fn type_script(target: &Target, text: &str, clear: bool, submit: bool) -> String {
    let (sel, r) = js_target(Some(target));
    let text = js_string(text);
    wrap(&format!(
        r#"    var sel = {sel}, ref = {r}, text = {text}, clear = {clear}, submit = {submit};
    var el = find(sel, ref);
    if (!el) return missing(sel, ref);
    if (el.disabled || el.readOnly) return fail('disabled', describe(el) + ' is not editable');
    el.scrollIntoView({{ block: 'center' }});
    if (el.focus) el.focus({{ preventScroll: true }});
    var tag = el.tagName;
    if (tag === 'SELECT') {{
      var opt = Array.prototype.find.call(el.options, function (o) {{ return o.value === text || o.label.trim() === text || o.text.trim() === text; }});
      if (!opt) return fail('no_option', 'No option "' + text + '" in ' + describe(el));
      el.value = opt.value;
    }} else if (tag === 'INPUT' || tag === 'TEXTAREA') {{
      var proto = tag === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      var setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
      setter.call(el, clear ? text : el.value + text);
      el.dispatchEvent(new InputEvent('input', {{ bubbles: true, composed: true, inputType: 'insertText', data: text }}));
    }} else if (el.isContentEditable) {{
      if (clear) {{ var range = document.createRange(); range.selectNodeContents(el); var s = getSelection(); s.removeAllRanges(); s.addRange(range); }}
      if (!document.execCommand('insertText', false, text)) {{ if (clear) el.textContent = ''; el.textContent += text; el.dispatchEvent(new InputEvent('input', {{ bubbles: true, composed: true }})); }}
    }} else {{
      return fail('not_editable', describe(el) + ' is not a text field');
    }}
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
    var what = describe(el);
    if (submit) {{
      var form = el.form || (el.closest && el.closest('form'));
      if (form) {{ if (form.requestSubmit) form.requestSubmit(); else form.submit(); }}
      else {{
        var k = {{ key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true, cancelable: true, composed: true }};
        el.dispatchEvent(new KeyboardEvent('keydown', k)); el.dispatchEvent(new KeyboardEvent('keypress', k)); el.dispatchEvent(new KeyboardEvent('keyup', k));
      }}
    }}
    return {{ ok: true, result: {{ ok: true, description: what }} }};"#
    ))
}

/// `page.press`: keydown/keypress/keyup on the element or the focused one.
/// Synthetic events do not trigger the browser's default actions, so Enter
/// in a form field submits the form and Tab moves the focus explicitly.
pub fn press_script(target: Option<&Target>, key: &str) -> String {
    let (sel, r) = js_target(target);
    let key = js_string(key);
    wrap(&format!(
        r#"    var sel = {sel}, ref = {r}, key = {key};
    var el = (sel || ref) ? find(sel, ref) : (document.activeElement || document.body);
    if (!el) return missing(sel, ref);
    if ((sel || ref) && el.focus) el.focus();
    var CODES = {{ Enter: 13, Tab: 9, Escape: 27, Backspace: 8, Delete: 46, ' ': 32, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40,
      Home: 36, End: 35, PageUp: 33, PageDown: 34 }};
    var code = CODES[key] || (key.length === 1 ? key.toUpperCase().charCodeAt(0) : 0);
    var o = {{ key: key, keyCode: code, which: code, bubbles: true, cancelable: true, composed: true,
      code: key.length === 1 ? (/[a-z]/i.test(key) ? 'Key' + key.toUpperCase() : (/[0-9]/.test(key) ? 'Digit' + key : '')) : (key === ' ' ? 'Space' : key) }};
    var go = el.dispatchEvent(new KeyboardEvent('keydown', o));
    if (go && (key.length === 1 || key === 'Enter')) go = el.dispatchEvent(new KeyboardEvent('keypress', o));
    if (go) {{
      if (key === 'Enter' && el.form && el.tagName === 'INPUT') {{ if (el.form.requestSubmit) el.form.requestSubmit(); else el.form.submit(); }}
      else if (key === 'Tab') {{
        var f = Array.prototype.filter.call(document.querySelectorAll('a[href],button,input,select,textarea,[tabindex]:not([tabindex="-1"]),[contenteditable=true]'), function (x) {{ return !x.disabled && visible(x); }});
        var i = f.indexOf(document.activeElement);
        var next = f[(i + 1) % f.length];
        if (next) next.focus();
      }} else if (key.length === 1 && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') && !el.readOnly) {{
        var proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, el.value + key);
        el.dispatchEvent(new InputEvent('input', {{ bubbles: true, composed: true, inputType: 'insertText', data: key }}));
      }}
    }}
    el.dispatchEvent(new KeyboardEvent('keyup', o));
    return {{ ok: true, result: {{ ok: true, description: key + ' on ' + describe(el) }} }};"#
    ))
}

/// `page.scroll`: an element into view, to the top or bottom, or by an
/// amount in CSS pixels.
pub fn scroll_script(target: Option<&Target>, dx: f64, dy: f64, to: Option<ScrollTo>) -> String {
    let (sel, r) = js_target(target);
    let num = |v: f64| if v.is_finite() { v.to_string() } else { "0".into() };
    let to = match to {
        Some(ScrollTo::Top) => "'top'",
        Some(ScrollTo::Bottom) => "'bottom'",
        None => "null",
    };
    wrap(&format!(
        r#"    var sel = {sel}, ref = {r}, to = {to};
    var what;
    if (sel || ref) {{
      var el = find(sel, ref);
      if (!el) return missing(sel, ref);
      el.scrollIntoView({{ block: 'center', inline: 'nearest' }});
      what = describe(el) + ' in view';
    }} else if (to === 'top') {{ window.scrollTo(0, 0); what = 'top'; }}
    else if (to === 'bottom') {{ window.scrollTo(0, (document.scrollingElement || document.documentElement).scrollHeight); what = 'bottom'; }}
    else {{ window.scrollBy({dx}, {dy}); what = 'by {dx},{dy}'; }}
    return {{ ok: true, result: {{ ok: true, description: what + ' (scrollY ' + Math.round(window.scrollY) + ')' }} }};"#,
        dx = num(dx),
        dy = num(dy)
    ))
}

/// One check of `page.wait`: `{found}` for a selector, a text, or the end of
/// loading (`document.readyState == 'complete'`).
pub fn wait_check_script(selector: Option<&str>, text: Option<&str>, load: bool) -> String {
    wrap(&format!(
        r#"    var sel = {sel}, text = {text}, load = {load};
    var found = true;
    if (load) found = document.readyState === 'complete';
    if (found && sel) {{ var el = find(sel, null); found = !!el && visible(el); }}
    if (found && text) found = ((document.body && document.body.innerText) || '').indexOf(text) >= 0;
    return {{ ok: true, result: {{ found: found }} }};"#,
        sel = js_opt(selector),
        text = js_opt(text)
    ))
}

/// How `page.eval` runs the user's script: first as an expression, and if
/// that does not even parse, as the body of an async function (where it
/// must `return` its value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalForm {
    Expression,
    Body,
}

/// The form to try first: scripts that are clearly statements (they
/// `return`, or start with a declaration or control keyword) go straight to
/// the body form, so the page never sees a syntax error from the
/// expression attempt.
pub fn first_eval_form(script: &str) -> EvalForm {
    let t = script.trim_start();
    let starts = ["const ", "let ", "var ", "if ", "if(", "for ", "for(", "while ", "while(", "try ", "try{", "throw ", "function ", "class ", "return", "await "];
    let has_return = t
        .match_indices("return")
        .any(|(i, _)| {
            let before = t[..i].chars().next_back();
            let after = t[i + 6..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
        });
    if has_return || starts.iter().any(|k| t.starts_with(k)) { EvalForm::Body } else { EvalForm::Expression }
}

/// Serialises a value to plain JSON (undefined → null, cycles → string).
const SER: &str = r#"function ser(v) { if (v === undefined) return null; try { var s = JSON.stringify(v); return s === undefined ? null : JSON.parse(s); } catch (e) { return String(v); } }"#;

/// `page.eval`. The script is inlined (not passed to `eval()`), so pages
/// whose Content-Security-Policy forbids `eval` still work. A promise
/// result is stored in the page and reported as `pending`.
pub fn eval_script(script: &str, form: EvalForm) -> String {
    let call = match form {
        // A trailing `;` is fine in a statement, not in an expression.
        EvalForm::Expression => {
            format!("(function () {{ return (\n{}\n); }})()", script.trim().trim_end_matches(';'))
        }
        EvalForm::Body => format!("(async function () {{\n{script}\n}})()"),
    };
    format!(
        r#"(function () {{ 'use strict';
  {SER}
  try {{
    var v = {call};
    if (v && typeof v.then === 'function') {{
      if (!window.__bcEval) Object.defineProperty(window, '__bcEval', {{ value: {{}} }});
      var id = 'p' + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
      window.__bcEval[id] = {{ done: false }};
      v.then(function (r) {{ window.__bcEval[id] = {{ done: true, value: ser(r) }}; }},
             function (e) {{ window.__bcEval[id] = {{ done: true, error: String(e && e.stack ? (e.message + '\n' + e.stack) : e) }}; }});
      return {{ ok: true, pending: id }};
    }}
    return {{ ok: true, result: {{ value: ser(v) }} }};
  }} catch (e) {{
    return {{ ok: false, code: 'script', error: String(e && e.stack ? (e.message + '\n' + e.stack) : e) }};
  }}
}})()"#
    )
}

/// Polls a pending `page.eval` promise.
pub fn eval_poll_script(id: &str) -> String {
    format!(
        r#"(function () {{
  var id = {id}, s = window.__bcEval && window.__bcEval[id];
  if (!s) return {{ ok: false, code: 'script', error: 'The page navigated away while the script was running' }};
  if (!s.done) return {{ ok: true, pending: id }};
  delete window.__bcEval[id];
  if ('error' in s) return {{ ok: false, code: 'script', error: s.error }};
  return {{ ok: true, result: {{ value: s.value }} }};
}})()"#,
        id = js_string(id)
    )
}

/// Reads (and empties) the page-side console buffer of
/// `console::console_capture_script`, for the headless mode.
pub fn console_buffer_script() -> &'static str {
    "(function () { var b = window.__bcConsoleBuf; if (!b) return { ok: true, result: [] }; var out = b.slice(); b.length = 0; return { ok: true, result: out }; })()"
}

/// What a script returned.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptOutcome {
    Done(Value),
    /// A promise is still running; poll with `eval_poll_script(id)`.
    Pending(String),
}

/// True when the evaluation produced nothing: the script did not parse or
/// the engine refused it (wry passes an empty string then).
pub fn did_not_run(json: &str) -> bool {
    json.trim().is_empty()
}

/// Parses a script's JSON completion value.
pub fn parse_script_result(json: &str) -> Result<ScriptOutcome, RpcError> {
    if did_not_run(json) {
        return Err(RpcError::new(RpcError::SCRIPT, "The script did not run (syntax error, or the page is not ready)"));
    }
    let v: Value = serde_json::from_str(json)
        .map_err(|e| RpcError::new(RpcError::INTERNAL, format!("unexpected script result: {e}")))?;
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => {
            if let Some(id) = v.get("pending").and_then(Value::as_str) {
                return Ok(ScriptOutcome::Pending(id.to_string()));
            }
            Ok(ScriptOutcome::Done(v.get("result").cloned().unwrap_or(Value::Null)))
        }
        Some(false) => {
            let message = v.get("error").and_then(Value::as_str).unwrap_or("script error").to_string();
            let code = match v.get("code").and_then(Value::as_str) {
                Some("no_element") => RpcError::NO_ELEMENT,
                Some("script") => RpcError::SCRIPT,
                Some("invalid_selector") => RpcError::INVALID_PARAMS,
                _ => RpcError::BROWSER,
            };
            Err(RpcError::new(code, message))
        }
        None => Err(RpcError::new(RpcError::INTERNAL, "unexpected script result")),
    }
}

/// What to do after a script answered.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Run this script now and feed its result to `on_result`.
    Run(String),
    /// Run this script after `POLL_INTERVAL` and feed its result to `on_result`.
    RunLater(String),
    /// Send this as the reply.
    Done(Result<Value, RpcError>),
}

#[derive(Clone, Debug)]
enum JobKind {
    /// One script, its result is the reply.
    Once,
    Eval { script: String, form: EvalForm },
    Wait { check: String },
}

/// A `page.*` request that is answered by running scripts in the page:
/// `page.text`, `page.html`, `page.snapshot`, `page.click`, `page.type`,
/// `page.press`, `page.scroll`, `page.eval`, `page.wait`.
#[derive(Clone, Debug)]
pub struct PageJob {
    kind: JobKind,
    started: Instant,
    deadline: Instant,
}

impl PageJob {
    /// The job for `method` and the first script to run, or None when the
    /// method is not answered by a page script (tabs, navigation,
    /// screenshots, console, …).
    pub fn start(method: Method, params: &Value) -> Result<Option<(PageJob, String)>, RpcError> {
        let started = Instant::now();
        let job = |kind, timeout_ms: u64| PageJob {
            kind,
            started,
            deadline: started + Duration::from_millis(timeout_ms),
        };
        let default_timeout = protocol::DEFAULT_TIMEOUT_MS;
        let (job, script) = match method {
            Method::PageText => {
                let p: ReadParams = parse_params(params)?;
                (job(JobKind::Once, default_timeout), text_script(p.selector.as_deref(), p.max_chars()))
            }
            Method::PageHtml => {
                let p: ReadParams = parse_params(params)?;
                (job(JobKind::Once, default_timeout), html_script(p.selector.as_deref(), p.max_chars()))
            }
            Method::PageSnapshot => {
                let p: SnapshotParams = parse_params(params)?;
                (job(JobKind::Once, default_timeout), snapshot_script(p.max_elements(), p.viewport_only))
            }
            Method::PageClick => {
                let p: ClickParams = parse_params(params)?;
                (job(JobKind::Once, default_timeout), click_script(&p.target()?))
            }
            Method::PageType => {
                let p: TypeParams = parse_params(params)?;
                (job(JobKind::Once, default_timeout), type_script(&p.target()?, &p.text, p.clear, p.submit))
            }
            Method::PagePress => {
                let p: PressParams = parse_params(params)?;
                if p.key.is_empty() || p.key.chars().count() > 32 {
                    return Err(RpcError::invalid_params("key must be a KeyboardEvent key such as Enter or a"));
                }
                (job(JobKind::Once, default_timeout), press_script(p.target()?.as_ref(), &p.key))
            }
            Method::PageScroll => {
                let p: ScrollParams = parse_params(params)?;
                (job(JobKind::Once, default_timeout), scroll_script(p.target()?.as_ref(), p.dx, p.dy, p.to))
            }
            Method::PageEval => {
                let p: EvalParams = parse_params(params)?;
                let form = first_eval_form(&p.script);
                let first = eval_script(&p.script, form);
                let kind = JobKind::Eval { script: p.script, form };
                (job(kind, protocol::timeout_ms(p.timeout_ms)), first)
            }
            Method::PageWait => {
                let p: WaitParams = parse_params(params)?;
                if p.selector.is_none() && p.text.is_none() && !p.load {
                    return Err(RpcError::invalid_params("give selector, text or load: true"));
                }
                let check = wait_check_script(p.selector.as_deref(), p.text.as_deref(), p.load);
                (job(JobKind::Wait { check: check.clone() }, protocol::timeout_ms(p.timeout_ms)), check)
            }
            _ => return Ok(None),
        };
        Ok(Some((job, script)))
    }

    /// Feeds the JSON the last script returned; says what to do next.
    pub fn on_result(&mut self, json: &str) -> Step {
        let timed_out = Instant::now() >= self.deadline;
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        match &mut self.kind {
            JobKind::Once => Step::Done(parse_script_result(json).and_then(|o| match o {
                ScriptOutcome::Done(v) => Ok(v),
                ScriptOutcome::Pending(_) => Err(RpcError::new(RpcError::INTERNAL, "unexpected pending result")),
            })),
            JobKind::Eval { script, form } => {
                if did_not_run(json) && *form == EvalForm::Expression {
                    *form = EvalForm::Body;
                    return Step::Run(eval_script(script, EvalForm::Body));
                }
                match parse_script_result(json) {
                    Ok(ScriptOutcome::Done(v)) => Step::Done(Ok(v)),
                    Ok(ScriptOutcome::Pending(_)) if timed_out => {
                        Step::Done(Err(RpcError::timeout(format!("the script's promise did not settle in {elapsed_ms} ms"))))
                    }
                    Ok(ScriptOutcome::Pending(id)) => Step::RunLater(eval_poll_script(&id)),
                    Err(e) if did_not_run(json) => Step::Done(Err(RpcError::new(
                        e.code,
                        "The script did not parse, as an expression or as a function body (use `return` to give a value)",
                    ))),
                    Err(e) => Step::Done(Err(e)),
                }
            }
            JobKind::Wait { check } => match parse_script_result(json) {
                Ok(ScriptOutcome::Done(v)) if v.get("found").and_then(Value::as_bool) == Some(true) => {
                    Step::Done(Ok(json!({ "found": true, "elapsed_ms": elapsed_ms })))
                }
                // An invalid selector will never match.
                Err(e) if e.code != RpcError::SCRIPT => Step::Done(Err(e)),
                // A page that is navigating answers nothing: keep waiting.
                _ if !timed_out => Step::RunLater(check.clone()),
                _ => Step::Done(Err(RpcError::timeout(format!("not found after {elapsed_ms} ms")))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_js_strings_are_escaped() {
        assert_eq!(js_string("a\"b"), r#""a\"b""#);
        assert_eq!(js_string("</script>\n"), r#""</script>\n""#);
        assert_eq!(js_string("x\u{2028}y\u{2029}"), "\"x\\u2028y\\u2029\"");
        assert_eq!(js_string("\\'"), r#""\\'""#);
        // Hostile input never closes the literal.
        let hostile = r#"'); alert(1); ('"); alert(2); //"#;
        let s = click_script(&Target::Selector(hostile.into()));
        assert!(s.contains(&js_string(hostile)));
        assert!(!s.contains(&format!("= '{hostile}")));
        let t = type_script(&Target::Ref("e1".into()), "\"};alert(1);//", true, false);
        assert!(t.contains(r#"text = "\"};alert(1);//""#));
    }

    #[test]
    fn automation_scripts_are_balanced() {
        // Cheap structural check: braces and parentheses outside strings
        // balance in every generated script.
        fn balanced(s: &str) -> bool {
            let (mut depth, mut quote, mut escape, mut regex_like) = (0i64, None::<char>, false, false);
            let _ = &mut regex_like;
            for c in s.chars() {
                if let Some(q) = quote {
                    if escape { escape = false; } else if c == '\\' { escape = true; } else if c == q { quote = None; }
                    continue;
                }
                match c {
                    '\'' | '"' => quote = Some(c),
                    '{' | '(' | '[' => depth += 1,
                    '}' | ')' | ']' => depth -= 1,
                    _ => {}
                }
                if depth < 0 { return false; }
            }
            depth == 0 && quote.is_none()
        }
        let t = Target::Selector("a[href='x']".into());
        for s in [
            text_script(Some("main"), 10),
            text_script(None, 10),
            html_script(None, 10),
            snapshot_script(10, true),
            click_script(&t),
            type_script(&t, "hi", true, true),
            press_script(None, "Enter"),
            press_script(Some(&t), "a"),
            scroll_script(None, 0.0, f64::NAN, Some(ScrollTo::Bottom)),
            wait_check_script(Some("#x"), Some("ok"), true),
            eval_script("1 + 1;", EvalForm::Expression),
            eval_script("return 2", EvalForm::Body),
            eval_poll_script("p1"),
            console_buffer_script().to_string(),
        ] {
            assert!(balanced(&s), "unbalanced script:\n{s}");
        }
        assert!(eval_script(" document.title; ", EvalForm::Expression).contains("return (\ndocument.title\n)"));
        assert!(scroll_script(None, 1.5, f64::INFINITY, None).contains("scrollBy(1.5, 0)"));
    }

    #[test]
    fn automation_script_results() {
        assert_eq!(parse_script_result(r#"{"ok":true,"result":{"a":1}}"#).unwrap(), ScriptOutcome::Done(json!({"a": 1})));
        assert_eq!(parse_script_result(r#"{"ok":true,"pending":"p1"}"#).unwrap(), ScriptOutcome::Pending("p1".into()));
        assert_eq!(parse_script_result(r#"{"ok":false,"code":"no_element","error":"x"}"#).unwrap_err().code, RpcError::NO_ELEMENT);
        assert_eq!(parse_script_result(r#"{"ok":false,"code":"script","error":"boom"}"#).unwrap_err().message, "boom");
        assert_eq!(parse_script_result("").unwrap_err().code, RpcError::SCRIPT);
        assert!(parse_script_result("42").is_err());
    }

    #[test]
    fn automation_page_jobs() {
        assert!(PageJob::start(Method::TabsList, &Value::Null).unwrap().is_none());
        assert!(PageJob::start(Method::PageClick, &json!({})).is_err());
        assert!(PageJob::start(Method::PageWait, &json!({})).is_err());

        let (mut job, script) = PageJob::start(Method::PageText, &json!({"selector": "main"})).unwrap().unwrap();
        assert!(script.contains(r#"var sel = "main""#));
        assert_eq!(job.on_result(r#"{"ok":true,"result":{"text":"hi"}}"#), Step::Done(Ok(json!({"text": "hi"}))));

        // Eval: expression first, then the body form when it does not parse,
        // then polling a promise.
        assert_eq!(first_eval_form("document.title"), EvalForm::Expression);
        assert_eq!(first_eval_form("x.returnValue"), EvalForm::Expression);
        assert_eq!(first_eval_form("const r = await fetch(u); return r.status"), EvalForm::Body);
        assert_eq!(first_eval_form("a = 1; return a"), EvalForm::Body);
        let (mut job, first) = PageJob::start(Method::PageEval, &json!({"script": "a = 1; a"})).unwrap().unwrap();
        assert!(first.contains("return (\na = 1; a\n)"));
        let Step::Run(body) = job.on_result("") else { panic!("no retry") };
        assert!(body.contains("async function () {\na = 1; a\n}"));
        let Step::RunLater(poll) = job.on_result(r#"{"ok":true,"pending":"p9"}"#) else { panic!("no poll") };
        assert!(poll.contains(r#"var id = "p9""#));
        assert_eq!(job.on_result(r#"{"ok":true,"result":{"value":1}}"#), Step::Done(Ok(json!({"value": 1}))));
        let (_, body_first) = PageJob::start(Method::PageEval, &json!({"script": "let a = 1; return a"})).unwrap().unwrap();
        assert!(body_first.contains("async function () {\nlet a = 1; return a\n}"));
        let (mut job, _) = PageJob::start(Method::PageEval, &json!({"script": "("})).unwrap().unwrap();
        assert!(matches!(job.on_result(""), Step::Run(_)));
        assert!(matches!(job.on_result(""), Step::Done(Err(e)) if e.code == RpcError::SCRIPT));

        // Wait: polls until found, gives up at the deadline.
        let (mut job, _) = PageJob::start(Method::PageWait, &json!({"selector": "#x", "timeout_ms": 50})).unwrap().unwrap();
        assert!(matches!(job.on_result(r#"{"ok":true,"result":{"found":false}}"#), Step::RunLater(_)));
        assert!(matches!(job.on_result(""), Step::RunLater(_)), "navigating page");
        assert!(matches!(job.on_result(r#"{"ok":true,"result":{"found":true}}"#), Step::Done(Ok(v)) if v["found"] == true));
        let (mut bad, _) = PageJob::start(Method::PageWait, &json!({"selector": "##"})).unwrap().unwrap();
        assert!(matches!(bad.on_result(r#"{"ok":false,"code":"invalid_selector","error":"x"}"#), Step::Done(Err(e)) if e.code == RpcError::INVALID_PARAMS));
        std::thread::sleep(Duration::from_millis(60));
        assert!(matches!(job.on_result(r#"{"ok":true,"result":{"found":false}}"#), Step::Done(Err(e)) if e.code == RpcError::TIMEOUT));
    }
}
