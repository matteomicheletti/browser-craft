# Manual tests

Checks that need a person: real accounts, keyboard focus under Wayland
(synthetic key events do not reach XWayland windows on GNOME), audio/video.
Record the result (OK / KO + note) and the date in the last column.

Rows marked "OK 2026-11-11 (nested X)" were run by the integration step on a
nested rootful Xwayland display (`Xwayland :7 -geometry 1400x900`, no window
manager) with `xdotool`, a temporary profile (`XDG_CONFIG_HOME`,
`XDG_DATA_HOME`, `HOME`) and `tests/web` served on 127.0.0.1. They still need
one pass on the real GNOME session: there the window manager gives the
keyboard focus, which the nested display does not (click the window first).

Build under test: `browsecraft --version`, installed by
`scripts/install-local.sh`.

## 1. Keyboard (inside a page and in the toolbar)

| # | Action | Expected | Result |
| --- | --- | --- | --- |
| 1.1 | Click inside a page, press Ctrl+T / Ctrl+W | New tab / tab closed | |
| 1.2 | Inside a page, Ctrl+L, type a URL, Enter | Address bar focused, page opens | |
| 1.3 | Inside a page, F5 / Ctrl+R | Page reloads | |
| 1.4 | Inside a page, Alt+Left / Alt+Right | Back / forward | |
| 1.5 | Inside a page, F12 and Ctrl+Shift+I | Web inspector opens | |
| 1.6 | Ctrl+P (and F1) | Command palette opens over a still picture of the page; typing filters; Up/Down + Enter runs the command; Esc closes | OK 2026-11-11 (nested X) |
| 1.9 | Ctrl+Shift+P | Print dialog opens; Cancel returns to the page | OK 2026-11-11 (nested X) |
| 1.10 | Ctrl+Shift+J | Automation drawer, Console view, with the page's errors | OK 2026-11-11 (nested X) |
| 1.11 | Ctrl+Shift+S on a web page (Linux) | PNG of the visible page in Pictures | OK 2026-11-11 (nested X) |
| 1.12 | Narrow the window below ~900 px | Containers, plugins, dark mode move into the ⋮ menu | OK 2026-11-11 (nested X) |
| 1.7 | In a text field of a page, Ctrl+Left/Right | Cursor moves by word (not history) | |
| 1.8 | Ctrl+1…Ctrl+9, Ctrl+PageUp/PageDown, Ctrl+Shift+T | Chrome behaviour | |

## 2. Logins and sessions (real accounts)

| # | Site | Check | Result |
| --- | --- | --- | --- |
| 2.1 | github.com | Log in with password + 2FA; close and reopen the browser: still logged in | |
| 2.2 | gitlab.com | Log in; open a merge request; comment box works | |
| 2.3 | Google account (accounts.google.com) | Log in; Google may refuse "unsupported browser": note the message | |
| 2.4 | Gmail / Google Drive / Docs | Inbox loads, a Doc opens and accepts typing | |
| 2.5 | Microsoft 365 (outlook.office.com, office.com) | Log in (incl. MFA/Authenticator push), Outlook web and a Word doc | |
| 2.6 | Teams web | Chat loads; note call/camera behaviour | |
| 2.7 | Atlassian (Jira/Confluence Cloud) | Log in with SSO; edit a page | |
| 2.8 | Bitbucket | Log in; open a PR diff | |
| 2.9 | A bank or PSD2 login | Only if you are comfortable; note any "browser not supported" | |
| 2.10 | Sites with SSO pop-ups (OAuth "Sign in with Google/Microsoft") | The pop-up opens as a tab and the login returns to the site | |
| 2.11 | Passkeys / WebAuthn (github.com → passkey) | Note whether WebKitGTK offers it | |
| 2.12 | Cookie banner + "remember me" | Choice persists after restart | |

## 3. Containers (after wave 1)

| # | Check | Result |
| --- | --- | --- |
| 3.1 | GitHub personal in container "Personal", work account in "Work", both open at once, no logout in either | |
| 3.2 | Restart: both sessions still there | |
| 3.3 | Temporary container: log in, close the tab, reopen in a new temporary container: logged out | |
| 3.4 | Rule "github.com → Work": opening github.com from the address bar lands in Work | |
| 3.5 | Tab colour/indicator shows the container | |
| 3.6 | Export containers, delete one, import: restored | |

## 4. Authenticator (after wave 1)

| # | Check | Result |
| --- | --- | --- |
| 4.0 | Shield button opens the drawer; create a vault; shield turns to a tinted check while unlocked | OK 2026-11-11 (nested X) |
| 4.1 | Add a test account from an `otpauth://` URI; code matches another app (e.g. phone authenticator) for the same secret | OK 2026-11-11 (nested X): matched an independent TOTP computation; phone app not tried |
| 4.6 | Copy a code; paste within 30 s, then after 30 s | Code, then nothing | OK 2026-11-11 (nested X, xclip) |
| 4.7 | Leave the vault unlocked with the drawer closed past the auto-lock time | Shield shows locked | |
| 4.2 | Import from a QR image | |
| 4.3 | Lock, restart, unlock with the master password | |
| 4.4 | Encrypted export, import on a fresh profile | |
| 4.5 | Real 2FA login on one site using the code | |

## 5. Media and the rest

| # | Check | Result |
| --- | --- | --- |
| 5.1 | YouTube: video plays with sound, fullscreen button | |
| 5.2 | Netflix/Spotify (DRM): note result (WebKitGTK usually has no Widevine) | |
| 5.3 | Download a file; upload a file in a form | |
| 5.4 | Camera/microphone permission (meet.jit.si) | |
| 5.5 | Print (palette → Print) | OK 2026-11-11 (nested X): dialog lists printers, Cancel; real printing not tried |
| 5.6 | Screen recording (palette) produces a playable file | OK 2026-11-11 (nested X): 6.6 s H.264 1280×800 MP4, frame shows the page; on the GNOME session check that x11grab sees the window |
| 5.7 | Dark mode on a light site, off on an excluded site | OK 2026-11-11 (nested X) for the toolbar toggle and live update; exclusion not tried |
| 5.9 | Palette → Screenshot of the whole page on a long page | Tall PNG in Pictures | OK 2026-11-11 (nested X) |
| 5.10 | Palette → Save page | `.mhtml` in Downloads that opens in a browser | OK 2026-11-11 (nested X): file written; reopening not tried |
| 5.11 | Palette → Clear browsing data, twice | History empty; logged-in site logged out | Partly 2026-11-11: history emptied; logout not tried |

## 5b. Plugins (after the integration)

| # | Check | Expected | Result |
| --- | --- | --- | --- |
| 5b.1 | Plugins drawer → Development folder `examples/plugins` → open https://example.com in a new tab | Hello World banner and word count | OK 2026-11-11 (nested X) |
| 5b.2 | Edit a dev plugin's JS | Notice within ~1 s; *Reload open tabs* shows the change | OK 2026-11-11 (nested X) |
| 5b.3 | Turn off the Dark mode built-in | Pages lose the dark theme at once; ◐ button gone | OK 2026-11-11 (nested X) |
| 5b.4 | A plugin calling `bc.notify` | Notice under the page | OK 2026-11-11 (nested X) |
| 5.8 | HiDPI: GNOME scaling 125%/200%: text sharpness, page size correct | |

## 6. Automation (headless, ctl, MCP)

Serve the probes first: `python3 -m http.server -d tests/web 8000`.

| # | Check | Expected | Result |
| --- | --- | --- | --- |
| 6.1 | `browsecraft headless eval http://127.0.0.1:8000/features.html 'new Promise(r => setTimeout(() => r(document.title), 3000))'` | `"value":"probe 17/17"`, exit 0 | OK 2026-11-11 |
| 6.2 | `browsecraft headless screenshot https://example.com /tmp/a.png --full-page` | PNG of the page, JSON with width/height | OK 2026-11-11 |
| 6.3 | `browsecraft headless pdf https://example.com /tmp/a.pdf` | 1-page PDF | OK 2026-11-11 |
| 6.4 | `browsecraft headless run http://127.0.0.1:8000/automation.html '[{"method":"page.type","params":{"selector":"#q","text":"hi"}},{"method":"page.click","params":{"selector":"#go"}},{"method":"page.text","params":{"selector":"#out"}}]'` | last result text `submitted hi r false` | OK 2026-11-11 |
| 6.5 | `browsecraft headless eval https://github.com 'const r = await fetch(location.href); return r.status'` (strict CSP) | `"value":200` | OK 2026-11-11 |
| 6.6 | `browsecraft --automation`, then `browsecraft ctl tabs.list` | the open tabs | OK 2026-11-11 (also navigate, text, snapshot, wait, screenshot, pdf, console.read) |
| 6.7 | `claude mcp add browsecraft -- browsecraft mcp`; ask Claude to open example.com and read it | tools called, page text returned | Partly 2026-11-11: scripted MCP stdio session (initialize, tools/list, browser_navigate, browser_text) OK; with Claude Code not tried |
| 6.8 | Close the browser; ask for a screenshot through MCP | the browser starts and answers | |
| 6.9 | `ls -l $XDG_RUNTIME_DIR/browsecraft` while the browser runs | `drwx------` dir, `srw-------` socket | Socket `srw-------` OK 2026-11-11 (custom path) |
| 6.10 | Automation drawer: tick *Allow running JavaScript*, `ctl page.eval '{"script":"6*7"}'`; untick, again | `42`, then permission denied | OK 2026-11-11 |
| 6.11 | Palette → *Toggle automation* twice | `ctl` fails (socket gone), then works | OK 2026-11-11 |
