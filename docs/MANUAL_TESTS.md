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
| 4.0 | QR-code button opens the drawer; create a vault; the button turns accent-coloured with an open padlock while unlocked | OK 2026-11-11 (nested X) |
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
| 5b.5 | Dev folder `~/Projects/browsecraft-plugins`, Clipboard manager on; in a page's text field press Ctrl+V | Modal over the dimmed page, newest item selected; Enter pastes it in the field; Esc closes and the keyboard returns to the field | OK 2026-11-11 (GNOME :0, hook `key:Ctrl+V`, `modal-js`, screenshots) |
| 5b.6 | Copy with `wl-copy` (text, `--type image/png`, `--type x-special/gnome-copied-files`, `--type text/html`) and `xclip -selection clipboard`, then Ctrl+V | Each shows up with its kind (image thumbnail, file names, formatted text); the same image re-owned by a clipboard manager is one item | OK 2026-11-11 (GNOME :0) |
| 5b.7 | Shift+Enter on an HTML or file item; Enter on an image item in a contenteditable | Plain text / file paths pasted; the image is inserted | OK 2026-11-11 (GNOME :0) |
| 5b.8 | Ctrl+L, then Ctrl+V, Enter | The item is pasted into the address bar | OK 2026-11-11 (GNOME :0, hook) |
| 5b.9 | Copy a password from the passwords page or a code from the authenticator, then Ctrl+V | Not in the history | (unit test; manual pending) |
| 5b.10 | Clipboard manager settings: *Open on Ctrl+V* off | Ctrl+V pastes normally; Ctrl+Shift+V still opens the manager | |
| 5b.11 | Change the theme with the modal open | The modal follows at once | OK 2026-11-11 (hook `theme-next`) |
| 5b.12 | Screenshot editor on, Ctrl+Shift+S on a page | Editor tab with the screenshot; draw arrow, rectangle, counter, pixelate; Ctrl+C puts the image on the clipboard; Ctrl+S saves to `~/Pictures/BrowseCraft` | OK 2026-11-11 (GNOME :0, hook; `wl-paste` gives the PNG) |
| 5b.13 | Palette → *Screenshot area…*, drag an area, Enter, draw, Ctrl+C | Editor over the page; the copied PNG has the area's size; the modal closes | OK 2026-11-11 (GNOME :0, hook) |
| 5b.14 | Plugins drawer | Clipboard manager shows the clipboard warning in red and "Takes Ctrl+V"; both plugins list pages and commands | OK 2026-11-11 (screenshot) |
| 5b.15 | Editor tools by mouse (pencil, line, marker, text, blur, invert, crop, select/move, wheel thickness) | As in Flameshot | Headless (synthetic pointer events, exported PNG checked); real mouse pending |
| 5.8 | HiDPI: GNOME scaling 125%/200%: text sharpness, page size correct | |

## 5c. Tab groups and split view

| # | Action | Expected | Result |
| --- | --- | --- | --- |
| 5c.1 | Right-click a tab → *Add tab to new group*, type a name, pick a colour | Coloured chip before the tab, underline under it, editor closes on a click outside | OK 2026-11-11 (nested X) |
| 5c.2 | Right-click another tab → *Add tab to group ▸* the group | The tab moves next to the group's tabs, underline covers both | OK 2026-11-11 (nested X) |
| 5c.3 | Click the chip; click it again | Collapsed to the chip with the tab count; expanded | OK 2026-11-11 (nested X) |
| 5c.4 | Collapse the group of the tab you are on (palette *Collapse or expand tab group*) | Moves to the nearest tab outside the group | OK 2026-11-11 (nested X) |
| 5c.5 | Collapse a group with an unloaded (dimmed) tab, expand it | The tab stays dimmed until shown | |
| 5c.6 | Tabs on the left (palette *Move tabs to top or side*) | Header rows with caret, chip and count; tabs indented with the group colour | OK 2026-11-11 (nested X) |
| 5c.7 | Chip right-click → *Ungroup*; another group → *Close group* | Tabs stay / tabs close (a new tab if they were all the tabs) | |
| 5c.8 | Right-click a tab → *Open in split view with current tab* | Both pages side by side, focused pane with a blue bar, pair outlined in the strip | OK 2026-11-11 (nested X) |
| 5c.9 | Drag the divider; double-click it | Panes resize live; back to half and half | OK 2026-11-11 (nested X, drag) |
| 5c.10 | Click in the other pane, then type in a field there | Address bar shows that page; keys go to the clicked page | Click OK 2026-11-11 (nested X); typing on GNOME to do |
| 5c.11 | Ctrl+F, Ctrl+plus, F12 in split | Act on the focused pane only | |
| 5c.12 | Open the containers drawer; open the palette | Both panes narrow; palette over pictures of both panes | OK 2026-11-11 (nested X) |
| 5c.13 | Palette *Swap split view panes*; tab menu *Close split view* | Panes swap; single page, both tabs still open | Swap OK 2026-11-11 (nested X) |
| 5c.14 | Ctrl+W on the focused pane | Split ends, the other tab becomes active | OK 2026-11-11 (nested X) |
| 5c.15 | Fullscreen a video in one pane (YouTube), Esc | Only that pane fills the screen; split comes back | |
| 5c.16 | Set *Unload background tabs after* to 1 min, wait 2 min with a split open | The other pane stays loaded | |
| 5c.17 | Quit and restart with groups and a split | Groups (names, colours, collapsed) and the split with its divider position come back | OK 2026-11-11 (nested X) |
| 5c.18 | Light theme | Group chips readable (white text on the darker shade) | OK 2026-11-11 (nested X) |
| 5c.19 | `browsecraft ctl tabs.list` with a group and a split | `group` and `split` fields on those tabs | OK 2026-11-11 (nested X) |

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

## 7. Window frame and themes (wave 3)

Rows marked "hook" were checked on the GNOME session through the debug-build
test hook (`BROWSECRAFT_WINDOW_SCRIPT`, see `docs/egui.md`), which runs the
same code as the buttons; moving and resizing need a real pointer press.

| # | Action | Expected | Result |
| --- | --- | --- | --- |
| 7.1 | Start with default settings | No system title bar; tabs and window buttons in one row; a thin border round the page | OK 2026-11-11 (GNOME, screenshot) |
| 7.2 | Drag the empty part of the tab strip (after `+`), and the toolbar with tabs in the sidebar | The window follows the pointer; on release, clicks in the page work at once | |
| 7.3 | Double-click the empty tab strip; again | Maximizes; restores | |
| 7.4 | Maximize / restore / minimize / close buttons | Each does what it says; the maximize glyph turns into restore | OK 2026-11-11 (hook: maximize 1920×1039, restore, minimize, close) |
| 7.5 | Right-click the empty title bar | Window menu: Minimize, Maximize (Restore), Close, each works | |
| 7.6 | Hover each edge and corner, drag | Resize cursor; the window resizes from that side; also next to a web page (left, right, bottom) | |
| 7.7 | Maximized: hover the top-right corner, click | No resize cursor; the close button closes the window | |
| 7.8 | `F11`, then a video's fullscreen button | No window buttons, no border, page fills the screen | OK 2026-11-11 (hook, `F11`) |
| 7.9 | `browsecraft://flags` → *Use system title bar* on, then off | The GNOME title bar appears; then the browser's frame comes back, without restart | OK 2026-11-11 (hook) |
| 7.10 | Toolbar theme menu, palette *Change theme…* (`%`), settings swatches: pick each theme | Chrome, open drawer, palette and internal pages change at once; the choice survives a restart | OK 2026-11-11 (hook + screenshots of all eight) |
| 7.11 | *Follow system*, switch GNOME Settings → Appearance light/dark, focus the window again | Light / Late Night follow | |


## 8. Windows, tab overview and hover cards (wave 4)

"hook" rows were checked on the GNOME session with `BROWSECRAFT_WINDOW_SCRIPT`
and screenshots; "nested" rows with real `xdotool` input on a nested
`Xwayland :7` without a window manager (GPU captures are black there, so the
outcome was read from the window list, the session file and the debug log).
What needs a person: the feel of the drag and of the hover card, and drags
between windows that a window manager places, on one or two monitors.

| # | Action | Expected | Result |
| --- | --- | --- | --- |
| 8.1 | `Ctrl+N`; title bar right-click → *New window* | A second window with a new tab, address bar focused | OK 2026-11-11 (hook `new-window`) |
| 8.2 | Right-click a tab → *Move to new window* | The tab opens in a new window **without reloading** (a page with a running counter keeps counting, its load time unchanged); the old window keeps the rest | OK 2026-11-11 (hook, `state.html` counter) |
| 8.3 | Right-click a tab → *Move to window ▸ Window 2* | The tab moves to the end of window 2 and is shown there; moving the last tab of a window closes that window | OK 2026-11-11 (hook) |
| 8.4 | Drag a tab along the strip | A blue line shows where it lands; on release the order changes; a click without moving still switches tab | OK 2026-11-11 (nested) |
| 8.5 | Drag a tab down into the page and release | A new window opens about where you let go, with the tab | OK 2026-11-11 (nested) |
| 8.6 | Drag a tab out of the window onto the desktop | A new window at the drop point | OK 2026-11-11 (nested; GNOME may shift it to keep it on screen) |
| 8.7 | Two windows side by side: drag a tab from one onto the other's tab strip, between two tabs | It lands there, in the other window, page kept | OK 2026-11-11 (nested, no window manager) |
| 8.8 | Same as 8.7 on GNOME, then with the windows on two monitors and with fractional scaling | As 8.7 (window positions come from X11; check they match what you see) | |
| 8.9 | Title bar right-click → *Name window…*, type "Research", Enter; reopen it → *Remove name* | The name shows before the window buttons, in the window title, in *Move to window* and in the overview; removed, it is "Window 1" again | OK 2026-11-11 (hook `name:`, dialog screenshot) |
| 8.10 | Two named windows, quit (`Ctrl+Q`), start again | Both windows come back with their tabs, names, places and sizes; unloaded tabs show their saved pictures in the overview | OK 2026-11-11 (hook, screenshots) |
| 8.11 | Close window 2 with its ✕, then `Ctrl+Shift+T` in window 1 | Window 2's tabs reopen one by one in window 1 | |
| 8.12 | `Ctrl+Shift+\` (also the grid button after `+`) | All tabs of every window as pictures, this window first, groups and the split pair together; the pages are hidden | OK 2026-11-11 (hook, screenshots) |
| 8.13 | In the overview type part of a title; arrows; `Enter` | Only matching tabs; the selection moves; Enter goes to the tab | |
| 8.14 | Overview: click a tab of the other window | That window comes to the front showing the tab (GNOME may only flash it in the dock if it refuses focus stealing) | |
| 8.15 | Overview: hover a tile → ✕; middle-click a tile; select one and `Ctrl+W` | Each closes that tab; the grid updates | |
| 8.16 | Rest the pointer on a background tab for half a second; move along the tabs; leave the strip | The card appears under the tab, over the live page, without the page blinking; it follows at once from tab to tab; it goes away on leaving | OK 2026-11-11 (hook `hover:`, card window above the page in the screenshot); feel to be checked by hand |
| 8.17 | Tabs in the left sidebar, hover a row | The card appears to the right of the sidebar | OK 2026-11-11 (hook) |
| 8.18 | `browsecraft://flags`: hover cards off; pictures off; delay 0.25 s / 1.2 s | No card; card without picture; the delay changes | |
| 8.19 | `browsecraft ctl tabs.list` with two windows; `tabs.open` with `"window": 2` | Each tab has `window: {id, name}`, a `windows` list; the tab opens in window 2; an unknown window id is an error | OK 2026-11-11 |

## User agent and Microsoft 365 (log in by hand)

For each row try the built-in choice (Safari on macOS) first, then Chrome and Edge from the palette (*User agent for this site…*), then Default.

| # | Action | Expected | Result |
| --- | --- | --- | --- |
| UA.1 | Sign in at https://outlook.cloud.microsoft/mail/ | Inbox loads; no "Something went wrong" / `OwaBasicUnsupportedException` | |
| UA.2 | Outlook calendar, open and send a mail | Works; Ctrl+B in the compose body makes text bold, the sidebar stays | |
| UA.3 | https://teams.microsoft.com (chat, a call preview) | No "unsupported browser" page | |
| UA.4 | Open a Word / Excel document from office.com or SharePoint | Document opens and edits | |
| UA.5 | Settings → Site user agents: remove an override | Row disappears; the site gets its automatic string on the next load | |
| SB.1 | Click in a page (not an editor), Ctrl+B twice; then in an editor | Library sidebar opens on the left (page narrows), then closes; in the editor Ctrl+B makes text bold | |
| SB.2 | Library: expand a nested folder, middle-click a bookmark, right-click a folder → Delete folder…, History → Load more, right-click → Remove from history, drag the edge, restart | New tab opens; folder deleted after confirming; more days load; row gone; width, section and open state come back | |
## 9. Import, history and bookmark folders (wave 5)

| # | Steps | Expected | Result |
|---|---|---|---|
| 9.1 | `browsecraft import --list` with Firefox and Chrome installed | JSON with each profile and its counts | OK 2026-11-11 (Firefox default-release, Chrome, Chrome Dev) |
| 9.2 | `browsecraft import firefox --dry-run`, then without `--dry-run`, then again | Same counts; the first run adds, the second adds nothing; source files untouched | OK 2026-11-11 (temp profile) |
| 9.3 | Run 9.2 while BrowseCraft is open | Bookmarks bar and pages update within ~2 s | |
| 9.4 | `browsecraft://import`: pick a profile, Import; then *Undo last import* | Progress, then a summary; undo removes it | |
| 9.5 | Drop a `bookmarks.html` on the window; export from the palette | Import page opens with the path; `~/Downloads/bookmarks-<date>.html` written | |
| 9.6 | Bookmarks bar folder click | Menu with subfolders as submenus | OK 2026-11-11 (hook `bar-folder:`) |
| 9.7 | Browse in a temporary container; open `browsecraft://history` | Those pages are not listed | |
| 9.8 | History page: search, *Older*, *Remove*, *Last hour*; restart | Changes persist across restarts | |

## 10. Site permissions and other browsers (wave 8)

| # | Steps | Expected | Result |
|---|---|---|---|
| 10.1 | https://webcamtests.com or a getUserMedia test page: start the camera | Bar "<host> wants to use your camera" under the toolbar, Remember ticked; Allow starts the camera (getUserMedia does not need WebRTC) | |
| 10.2 | Reload 10.1 | No bar: the saved allow applies; *Settings → Site permissions* lists it; *Reset* there, reload: the bar again | |
| 10.3 | Untick Remember, Block; reload | Blocked once, asked again after the reload | |
| 10.4 | Ask for the camera, then navigate the tab elsewhere (or close it) before answering | Bar disappears; the request is denied | |
| 10.5 | Request in a background tab, switch to it | The bar shows there, not in the other tab | |
| 10.6 | Screen sharing test page (getDisplayMedia) | "wants to share your screen", "Asked every time"; an Allow is not saved | |
| 10.7 | Palette `!` on a site: Camera: Block; then *Reset site permissions* | Notice; the site asks again | |
| 10.8 | Open teams.microsoft.com on the distro WebKitGTK (no WebRTC) | One-time bar "Calls need WebRTC…" with "Open in <browser>"; the button opens the page there; not shown again this run | |
| 10.9 | Tab menu *Open page in another browser* on a web page and on `browsecraft://flags` | Opens in the default browser / notice "Only web pages…" | |
| 10.10 | Location (maps site) and notifications (`Notification.requestPermission()` in the console) | Same bar with the right text; answers saved per host | |
## 10. Session presets (wave 8)

| # | Steps | Expected | Result |
|---|---|---|---|
| 10.1 | Two windows with a group (collapsed), a split pair, a container tab and a temporary-container tab; title bar menu → *Save session as preset…* → "Lavoro", All windows | Notice "Saved preset"; `browsecraft://sessions` lists "Lavoro · 2 windows, N tabs" (temporary tab not counted) | |
| 10.2 | Close everything but one tab; palette `$lav` → Enter | Two new windows with their names, groups (collapsed), split pair, container tabs; only the shown tabs load | |
| 10.3 | Palette `$lav` → Shift+Enter (or the *add to this window* row) | The preset's tabs are appended after the current ones; groups recreated; split only if the window had none | |
| 10.4 | `browsecraft://sessions` → *Replace this window…* → confirm | The window's tabs close (Ctrl+Shift+T reopens them), the preset's tabs and window name take their place | |
| 10.5 | Delete the container used by a preset tab, open the preset | The tab opens without container; notice says so | |
| 10.6 | Sessions page: rename, ↑/↓, *Update…* (confirm), *Delete…* (confirm), Export, Import the exported file | Each change persists across restarts; import adds "Lavoro (2)" | |
| 10.7 | Tab strip `▾` menu → *Open preset in new window ▸* | The preset opens in new windows | |
| 10.8 | Settings → On startup: *Open a preset* → pick "Lavoro"; restart; delete it; restart | The preset opens; after deleting, the previous session opens with a notice | |
| 10.9 | On startup: *Ask every time*; restart; try *Previous session*, a preset, *Blank page* | The chooser shows; each choice replaces the chooser tab (previous session's other windows open too) | |
| 10.10 | `browsecraft --preset Lavoro`; `browsecraft --preset Nope`; `browsecraft --preset Lavoro example.com` | The preset; the previous session with a notice; only example.com | |
| 10.11 | `browsecraft ctl sessions.list`; `sessions.save {"name":"X","scope":"all"}`; `sessions.open {"name":"x","window":"current"}`; with `allow_navigation = false` | Lists; saves (taken name is an error); opens; open/save refused | |
