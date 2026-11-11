# Usage

## Starting the browser

```sh
browsecraft                 # opens Startpage (the home page)
browsecraft example.com     # opens a URL
browsecraft "rust browser"  # runs a search
browsecraft --screenshot out.png example.com 1200   # renders to a PNG
browsecraft --container Work mail.example.com example.org   # two tabs, the first in "Work"
browsecraft --temporary-container example.com              # a tab in a new temporary container
```

Each address on the command line opens in its own tab; `--container <name>` (a container's name or id) or `--temporary-container` applies to the address that follows it (alone, to a start-page tab). An unknown container name is an error (exit code 2). Without an address, BrowseCraft reopens the previous session's tabs (see *Tabs*) or the home page.

The command-line argument is read the same way as text typed into the address bar. `--screenshot <out.png> <url> [width]` renders the page with BrowseCraft's own engine (never WebView2) and exits; the width defaults to 1200 pixels.

## Address bar

Press `Enter` to go. BrowseCraft decides what the text means using these rules, in order:

| Input                                                  | Result                  |
| ------------------------------------------------------ | ----------------------- |
| Has a scheme (`https://…`, `ftp://…`, `browsecraft://…`)     | Opened unchanged        |
| Starts with `about:`, `data:`, `file:`, `view-source:` | Opened unchanged        |
| `localhost`, an IPv4 address or a bracketed IPv6 address (`[::1]`), with an optional port | `http://` is added      |
| A domain such as `docs.rs/wry` or `example.com:8443`   | `https://` is added     |
| Anything else, including text with spaces              | Startpage search        |

A domain needs a dot and a top-level part of at least two letters. So `hello` and `1.5` are searched, not opened. Searches go to Startpage, the default search engine.

The address bar supports selection with the mouse or `Shift` + arrows, `Ctrl` + arrows to move by word, and `Ctrl+A`, `Ctrl+C`, `Ctrl+X` and `Ctrl+V`. Press `Esc` while editing to put back the current address.

The lock at the start of the address bar means the page shown arrived over HTTPS with a valid certificate. It follows the page actually loaded, not what you type, and it is not shown on BrowseCraft's own notice and error pages, or for a site whose certificate warning you clicked through.

## Where pages render

On Windows, web pages (`http:`, `https:`, `file:` and `data:` addresses) are shown in an embedded WebView2 pane (WKWebView on macOS). BrowseCraft's own engine renders the built-in pages (`about:`, `browsecraft://` and `view-source:`), the result of a POST form submitted from an engine page, and every page when WebView2 is unavailable or on other platforms. A Linux build with the experimental `servo` feature shows web pages with Servo instead; there, pop-ups are not opened and the password manager cannot yet detect or fill forms.

This matters for input. BrowseCraft's shortcuts below also work while a WebView2 page has the keyboard (handled on Windows through the WebView2 controller's `AcceleratorKeyPressed` event). Forms, link hover, `Ctrl`+wheel and the scrolling keys described here apply to engine pages; WebView2 pages handle these themselves.

## Built-in pages

| Page                  | What it shows                                                     |
| --------------------- | ----------------------------------------------------------------- |
| `about:home`          | The start page that new tabs open                                 |
| `about:blank`         | An empty page                                                     |
| `browsecraft://flags`       | Settings (see below)                                              |
| `browsecraft://history`     | Pages visited this session, newest first                          |
| `browsecraft://bookmarks`   | Your bookmarks, with a Remove button each                         |
| `browsecraft://passwords`   | The password vault (see Password manager below)                   |
| `view-source:<url>`   | The raw source of an `http:`, `https:` or `file:` page            |

History is kept in memory only (up to 500 entries) and is lost when BrowseCraft quits. Built-in pages and addresses longer than 8 KB are not recorded. When the engine renders a `file://` URL that points at a folder, it shows a directory listing; a drive on its own (`file:///C:`) is refused, use `file:///C:/` for its root.

BrowseCraft drops a user name and password written into an address (`https://user:password@example.com/`) before loading it: they are never sent, shown in the address bar or kept in the history. A site that needs them asks for them instead (WebView2 shows its sign-in prompt). This also unmasks addresses such as `https://bank.example@evil.example/`, which loads `evil.example`.

## Toolbar

| Button  | Action                                  |
| ------- | --------------------------------------- |
| ◀       | Back                                    |
| ▶       | Forward                                 |
| ↻ / ✕   | Reload, or stop while a page is loading |
| ⌂       | Home (Startpage)                        |
| Palette | Theme menu: pick one of the themes (see *Themes*) |
| Work    | The active tab's container, in its colour (only for container tabs); click to manage containers |
| ≋       | Containers panel (right-hand drawer)    |
| ◐       | Dark mode for this site: click to turn it on or off (tinted while the site is dark); right-click for the dark mode panel |
| Puzzle  | Plugins panel                           |
| QR code | Authenticator panel; tinted, with an open padlock in its corner, while the authenticator is unlocked (a closed padlock while locked) |
| ● Stop  | Shown while a screen recording runs: stops it |
| ⚙       | Open settings (`browsecraft://flags`)         |
| 🔑      | Password manager menu (top right)       |

On a narrow window the containers, plugins and dark mode buttons move into a
`⋮` menu (with the automation panel and the command palette), so the address
bar keeps its room. A built-in turned off in the Plugins panel (dark mode,
authenticator) loses its button.

The star inside the address bar bookmarks the page you are on (click it again to remove the bookmark); it is also `Ctrl+D`.

A bar along the bottom of the toolbar shows that a page is loading. On engine pages, hovering a link shows its address in the bottom-left corner.

## Window and title bar

BrowseCraft draws its own window frame, as Chrome does. With the tabs on top,
the tab strip is the title bar: the window buttons (minimize, maximize or
restore, close) sit at its right end, and the empty part of the strip (there is
always a free stretch after the `+`) moves the window when you drag it.
Double-click it to maximize or restore the window; right-click it for a small
window menu (New window, Name window…, Minimize, Maximize / Restore, Close). With the tabs in a sidebar
or hidden, the toolbar is the title bar instead, with the window buttons after
the password key. Resize the window from any edge or corner: a thin border
(5 pixels) round the page is the grip, and the pointer changes shape over it.
A maximized window has no border. In full screen (`F11`, or a page's video in
full screen) the window buttons and the border go away.

To use the desktop's own title bar and frame instead, turn on *Use system
title bar* on `browsecraft://flags`; it applies at once. The software
interface (`chrome=engine`) always uses the desktop's frame.

## Themes

The theme sets the colours of the whole browser: title bar, tabs, toolbar,
address bar, bookmarks bar, menus, notices, the drawer panels, the command
palette and BrowseCraft's own pages (the new-tab page, settings, history,
bookmarks, passwords). There are eight:

| Theme | Kind | Look |
| --- | --- | --- |
| Late Night (default) | Dark | Deep navy and black, blue accent |
| Slate Gray | Dark | Cool grey, steel-blue accent |
| Olive | Dark | Dark olive, leaf-green accent |
| Frost | Dark | Nord-like blue-grey, ice-blue accent |
| Rosé | Dark | Deep purple, rose accent |
| Forest | Dark | Deep green, teal accent |
| Light | Light | White and light grey, blue accent |
| Sand | Light | Warm beige, burnt-orange accent |

*Follow system* uses Light or Late Night as the desktop is set (GNOME's
*Appearance* setting on Linux; it is checked again when the window gets the
focus). Pick a theme from the palette button in the toolbar, from the command
palette (*Change theme…*, or type `%` followed by part of a name) or on
`browsecraft://flags`; the change applies at once, without a restart, and is
saved. Web pages that follow the system preference, and dark mode for pages
set to follow the browser, use the theme's kind (light or dark).

## Tabs

Tabs sit in a strip above the address bar, as in Chrome and Firefox: each shows
the site's icon, the title and a ✕ (on the active or hovered tab), and tabs in a
container carry the container's colour along their top edge. Tabs shrink as you
open more and the strip scrolls when they no longer fit. Click a tab to switch to
it, middle-click or click its ✕ to close it, `+` opens a new tab, and the `⌄`
next to it opens a new tab in a container (or a new temporary container).
Right-clicking a tab offers *Duplicate tab*, *New tab in container*, *Reopen in
container* (the page moves to another container), *Close tab* and *Close other
tabs*, plus *Move to new window* and *Move to window ▸* (see
[Windows](#windows)). Drag a tab along the strip to reorder it (a blue line shows
where it lands). New tabs open `about:home` with the address bar focused.
Closing the last tab closes its window, and closing the last window quits
BrowseCraft. On `browsecraft://flags` the tabs can instead be a
vertical sidebar on the left or right (with the container colour as the bar at
the start of each row), or hidden. The software interface (`chrome=engine`)
always uses the sidebar.

Each tab keeps its own web page alive, so switching tabs does not reload it and
its state (scroll position, forms, media) stays as you left it. To save memory,
background tabs are unloaded after a while (30 minutes by default) and the least
recently used ones are unloaded when more than 16 web pages are open; a tab
playing sound is never unloaded (Linux). An unloaded tab is shown dimmed and
loads its page again when you switch to it. Both limits are on
`browsecraft://flags` (*Tabs and memory*).

BrowseCraft remembers the open tabs and reopens them at the next start, each in
its container; only the tab you were on loads at once, the others when you first
open them. Tabs of temporary containers are not remembered. Turn this off with
*Reopen tabs at start-up* on `browsecraft://flags` (the session file,
`session.json` in the [per-user settings folder](../README.md#settings), is then
deleted). `Ctrl+Shift+T` reopens the last closed tabs (up to 25), with their
history and container.

### Tab hover cards

Rest the pointer on a tab (in the strip or the sidebar) and, after half a
second, a card shows its title, its site, its container and group, whether it
is unloaded or in the split view, and a picture of the page (Chrome's and
Edge's hover cards). Moving to another tab swaps the card at once; leaving the
tabs hides it, and coming back within a moment shows the next one without the
wait. A click or a drag on the tab hides it. The tab you are on shows no
picture (it is on screen). The picture is taken when you leave a tab and a
moment after a page loads; an unloaded tab keeps its last picture. On
`browsecraft://flags` (*Layout*) you can turn the cards off, turn their
pictures off, and set the delay (none, 0.25, 0.5, 0.8 or 1.2 seconds).

The card floats over the web page: it is its own small window, stacked above
the page, so the page does not need to be hidden while it shows (Linux).

### All tabs (overview)

`Ctrl+Shift+\` (Safari's `Cmd+Shift+\`), the grid button at the end of the tab
strip, or *Show all tabs* in the palette shows every tab of every window as a
grid of page pictures, like Safari's tab overview. The window you are in comes
first, then the other windows, each under its name (or *Window 2*, …); inside
a window, a group's tabs are together under the group's name and colour, and
the split view's two tabs sit side by side in one outline. Unloaded tabs are
dimmed; a tab without a picture yet shows its icon and site.

- Type to filter by title or address (every word must match).
- Click a tab, or select it with the arrow keys and press `Enter`, to go to
  it; a tab of another window brings that window to the front.
- The ✕ on a tile (hover or select it), a middle click, or `Ctrl+W` on the
  selected tile closes the tab.
- The *New tab* tile opens a tab in this window.
- `Esc` clears the search, then leaves; *Done* leaves.

The web pages are hidden while the overview shows; any other browser shortcut
leaves it and runs.

### Tab groups

Groups keep related tabs together under a name and a colour, as in Chrome and
Edge. Right-click a tab and choose *Add tab to new group*: the group's chip
appears before the tab with an editor where you type its name and pick one of
nine colours. *Add tab to group ▸* moves a tab into an existing group (it moves
next to the group's other tabs) and *Remove from group* takes it out. In the
strip a group is a coloured chip followed by its tabs, underlined in its colour;
in the vertical sidebar it is a header row with its tabs indented under it.

Click a group's chip (or header row) to collapse it to the chip alone, showing
how many tabs it hides, and again to expand it. Collapsing the group of the tab
you are on moves you to the nearest tab outside it (a new tab when there is
none). Collapsing does not load or unload anything: unloaded tabs stay unloaded.
Right-click the chip to rename it, recolour it, open a *New tab in group*,
*Collapse* / *Expand*, *Ungroup* (the tabs stay) or *Close group* (its tabs
close). A group can mix tabs of different containers. Groups, their colours
and whether they are collapsed are remembered with the session.

### Split view

Split view shows two tabs side by side, each with its own live page. Right-click
a tab and choose *Open in split view with current tab*, or use the palette's
*Split view with tab…* and pick the tab: the current tab goes on the left, the
one you picked on the right. The two tabs are outlined together in the strip.

The focused pane has a blue bar along its top: the address bar, back, reload,
find (`Ctrl+F`), zoom and developer tools act on it. Click in a page to focus
that pane. Drag the divider between the panes to resize them (double-click it to
split the width in half). Right-click either tab for *Swap split panes* and
*Close split view* (both tabs stay open); the palette also has *Focus the other
split pane*. Switching to a tab outside the pair shows that tab alone; going
back to either tab of the pair shows the split again. Closing one of the pair
ends the split and focuses the other. A drawer opened on the right narrows both
panes. A page that goes fullscreen (a video) fills the screen alone. The pane
on screen is never unloaded to save memory. A tab showing a BrowseCraft page
(`about:home`, `browsecraft://…`) in the other pane is shown as a placeholder:
click it to focus that tab. Split view needs the GPU interface. The pair and the
divider position are remembered with the session.

## Windows

BrowseCraft can have several windows. `Ctrl+N`, *New window* in the palette or
in the title bar's right-click menu opens one with a new tab. Each window has
its own tabs, groups, split view, drawer and palette; the containers (and their
logins), bookmarks, history, settings and the automation server are shared.

**Moving tabs between windows.** Right-click a tab: *Move to new window* puts
it in a window of its own, *Move to window ▸* lists the other windows. Or drag
it: along the strip to reorder, down out of the strip (or out of the window)
to tear it off into a new window where you let go, or onto another window to
move it there (onto that window's tab strip it lands where you drop it,
anywhere else at the end). A moved tab keeps its page as it was (scroll
position, forms, a playing video, a logged-in session): nothing reloads. It
leaves its tab group and the split view. When a window's last tab leaves, the
window closes.

**Naming windows.** Windows are unnamed (*Window 1*, *Window 2*, … by
position). Right-click the title bar and choose *Name window…* (or *Name
window…* in the palette) to give one a name such as *Work* or *Research*; it
shows quietly before the window buttons, in the window's title, in *Move to
window*, in the overview and in the automation API. *Remove name* goes back to
the default.

**Closing.** Closing a window (its ✕, `Ctrl+W` on its last tab) closes its
tabs; they go on the closed-tab list, so `Ctrl+Shift+T` in any window reopens
them one by one. Closing the last window, or `Ctrl+Q`, quits. The session
remembers every window: its tabs, groups, split view, name, place and size.

## Containers

Containers keep sites apart: each container has its own cookies, logins,
`localStorage`, IndexedDB, cache and service workers, so you can be signed in to
two accounts of the same site in two tabs, or keep a shopping site away from
everything else. A tab with no container uses the default profile.

- **Create, edit, delete**: the containers button in the toolbar (or *Manage
  containers* in the `⌄` menu) opens the Containers panel on the right. Give a
  container a name, one of nine colours and an icon. Deleting a container closes
  its tabs and deletes its cookies and site data.
- **Open a tab in a container**: the `⌄` next to `+`, the `+` beside a container
  in the panel, a tab's right-click menu, or `--container <name>` on the command
  line.
- **Always open in**: in the panel's *Always open in* section, add a site (it
  suggests the current one) and pick a container. The site and its subdomains
  then always open in that container: typing its address, a bookmark or a link
  in another container's page opens it in a new tab of the right container (in
  the same tab when it is a fresh, empty one). The most specific rule wins
  (`mail.example.com` over `example.com`).
- **Temporary containers**: `Ctrl+Shift+N`, *Temporary* in the panel or *New
  temporary container* in the `⌄` menu open a tab in a new, empty container
  ("Temporary 1", orange). Its data lives in a private folder under
  `$XDG_RUNTIME_DIR` that is deleted when its last tab closes (and at the next
  start if BrowseCraft crashed).
- **Import / export**: the panel writes or reads a JSON file (the path field,
  `~/browsecraft-containers.json` by default), or copies / pastes the same JSON
  through the clipboard. Importing updates containers with the same id and adds
  the others; rules for the same site are replaced. Temporary containers are
  never exported.

Containers and rules are saved in `containers.json` in the
[per-user settings folder](../README.md#settings); each container's web data is
in `~/.local/share/BrowseCraft/profiles/container-<id>`. Isolation is done by
the web engine (WebKitGTK's website data manager, one per container), on Linux.
On Windows each container gets its own WebView2 profile folder (untested); on
macOS containers are not isolated yet.

## Drawer

Panels open in a drawer on the right of the page; the page narrows to make room
(it is never covered). Click the panel's ✕ or its toolbar button again to close
it. The panels are Containers, Authenticator, Dark mode, Plugins and Automation
(with the JavaScript console).

## Command palette

`Ctrl+P` (`Cmd+P` on macOS) or `F1` opens the command palette over the page;
the same key, `Esc` or a click outside closes it. Type to search every browser
action (with its shortcut), the open tabs (with their container colour), your
bookmarks and recent history, or type an address or search terms to open them
in a new tab. `Up` / `Down` choose, `Enter` runs, `Tab` jumps to the next
<<<<<<< HEAD
section. Prefixes narrow the search: `>` commands only, `@` open tabs and tab
groups, `#` bookmarks and history, `~` open a tab in a container, `|` split view
with an open tab, `&` add the tab to a group, `?` lists them. Recently run
commands come first.
=======
section. Prefixes narrow the search: `>` commands only, `@` open tabs, `#`
bookmarks and history, `~` open a tab in a container, `%` themes, `?` lists
them. Recently
run commands come first.
>>>>>>> wave3/chrome

Commands include: new tab (also in a container or a temporary container),
duplicate, close, close others, reopen, next / previous tab, move the tabs to
the top or the side; new tab group, add tab to group, remove tab from group,
collapse or expand tab group; split view with tab, swap split view panes,
focus the other split pane, close split view; back, forward, reload, reload bypassing the cache, home,
focus the address bar, copy the address; find, zoom, fullscreen, print, save
the page (an `.mhtml` file in `~/Downloads`), view source; screenshot of the
visible page or the whole page (PNG in Pictures), start / stop a screen
recording (MP4 in `~/Videos`, needs `ffmpeg`); developer tools, JavaScript
console, automation server on / off; bookmark, bookmarks bar, bookmarks page,
history, clear browsing data; containers, authenticator, passwords; change
theme; dark mode for pages / this site / settings; plugins, reload plugins;
settings; quit.

*Clear browsing data* asks you to run it a second time within 10 seconds, then
deletes the history and the cookies, cache and site data of the default profile
and of every container with an open tab.

## Authenticator

The QR-code button opens the authenticator: two-factor codes (TOTP and HOTP)
for your accounts, in a vault encrypted with its own master password, separate
from the password manager. Create the vault the first time, then *Add account*
with the setup key, an `otpauth://` link (or a Google Authenticator export
link) or a QR image. Each account shows its current code with a countdown;
click the copy button to put it on the clipboard (it is taken off after 30
seconds). The vault locks itself after 5 minutes without use (change it in the
panel's settings), also while the panel is closed; the toolbar button shows
whether it is unlocked (accent colour and an open padlock) or locked (a closed
padlock). Pages, plugins and the automation API can never read
it. Formats and security: [AUTHENTICATOR.md](AUTHENTICATOR.md).

## Dark mode

Web pages get a dark theme computed from their own colours (Dark Reader's
engine), by default whenever the browser theme is dark. The ◐ button turns it
on or off for the current site; right-click it, or use the palette, for the
panel: on/off, mode (follow the browser, always, never), per-site *Always
dark* / *Never* / *Excluded*, custom CSS per site, brightness, contrast,
sepia, grayscale, font and a daily schedule. Changes apply to open pages at
once, without reloading. Pages that are already dark are left alone. Details:
[DARKMODE.md](DARKMODE.md).

## Plugins

The puzzle button opens the plugin manager: install a plugin folder, add a
development folder (plugins used in place and reloaded when their files
change), turn plugins and the built-ins on or off, change their settings and
see their permissions. New tabs use the change at once; open tabs keep the
scripts they were loaded with until you click *Reload open tabs* in the panel.
How to write one: [EXTENSIONS.md](EXTENSIONS.md); examples in
`examples/plugins/`.

## Automation and JavaScript console

The Automation panel (palette: *JavaScript console* or `Ctrl+Shift+J`;
*Automation and console* in the `⋮` menu) has two views:

- **Console**: what the current tab logged (`console.log`, warnings, errors,
  uncaught errors and rejections, plugin `bc.log` lines), newest first, with a
  level filter and *Clear*. The same entries are what `console.read` returns.
- **Control server**: turns the local control API on or off (also
  `--automation` for one run, or the palette's *Toggle automation*), allows
  navigation, `page.eval` (off by default) and `file:` URLs, limits automation
  to some sites, and shows the socket path and the command that adds the MCP
  server to Claude Code (`claude mcp add browsecraft -- <path>/browsecraft
  mcp`). Changes apply at once.

With the server on, `browsecraft ctl tabs.list`, `browsecraft ctl
page.navigate '{"url":"https://example.com"}'`, `browsecraft ctl page.text`
or `page.screenshot` drive the running browser from a shell, and
`browsecraft mcp` lets an agent do the same. See
[AUTOMATION.md](AUTOMATION.md).

## Find in page

`Ctrl+F` opens the find bar under the toolbar on web pages: type to search
(case-insensitive, wrapping), `Enter` / `Shift+Enter` or the arrows for the
next / previous match, `Esc` or ✕ to close. On Linux every match is highlighted
and the bar shows how many there are.

## Developer tools

`F12` or `Ctrl+Shift+I` open WebKit's web inspector (elements,
console, sources, network, storage...) docked at the bottom of the page on
Linux. Its ✕ closes it. Detaching it into its own window from its toolbar does
not work in this embedding yet. `BROWSECRAFT_DEVTOOLS=1` opens it for every new
tab.

## Bookmarks

Click the star in the address bar, or press `Ctrl+D`, to bookmark the current page; do it again to remove it. Bookmarks show on the bookmarks bar under the address bar (turn it off in Settings or with `Ctrl+Shift+B`). Click one to open it, middle-click to open it in a new tab, right-click for *Open in new tab* and *Remove bookmark*. Bookmarks that do not fit go in the `»` menu. **All bookmarks** (`Ctrl+Shift+O`) opens `browsecraft://bookmarks`. They are saved in `bookmarks.txt` in the [per-user settings folder](../README.md#settings) (one `url<TAB>title` per line). Only `http(s)` pages can be bookmarked. The bar is drawn by the GPU interface; with the software interface (`chrome=engine`) bookmarks still work from `Ctrl+D` and the manager page.

The key menu, the bookmarks bar and the save-login bar belong to the GPU interface (the default). When a menu is open over a web page, the page is shown as a still picture until the menu closes (the live page is a native window that cannot sit under a menu).

## Fullscreen

When a web page goes fullscreen, for example a video's fullscreen button on YouTube, the whole window goes fullscreen and the toolbar, bookmarks bar and tabs are hidden, so the page fills the screen. For the first few seconds a strip at the bottom, drawn by BrowseCraft rather than the page, names the site and how to leave. Press `Esc`, `F11` or the page's own button to leave; `F11` always works, even if the page handles `Esc`. The window returns to how it was. Outside page fullscreen, `F11` makes the window fullscreen on its own.

## Password manager

Click the key icon at the top right. The first time, choose how to protect your vault:

- **A password**, or
- **A key file**: a small file with random bytes that BrowseCraft creates for you. The vault cannot be opened without it. Keep it apart from the vault (a USB stick, say). If you lose it, the vault is gone for good. BrowseCraft creates it readable by your user account only, even in a shared folder.

The vault is a standard KeePass 4 (`.kdbx`) file, so KeePassXC and other KeePass apps open it too. You can also open an existing vault. BrowseCraft locks the vault again after 15 minutes without use, and keeps which vault to use (never a password) in `passwords.cfg` in the [per-user settings folder](../README.md#settings), which is also where it suggests creating a new vault.

On a page with a login form the key menu lists the logins saved for that site: click one to fill the page. A login counts for its own host and for hosts above or below it in the same registered domain (a login for `example.com` is offered on `login.example.com`), but not for sibling subdomains or across a public suffix such as `github.io`. On a sign-up or change-password form, *Suggest a strong password* fills a generated one. When you submit a login for a new account, a bar offers to **Save** it, or to never ask for that site; a change-password form offers to **Update** the saved password. Signing in with an account that is already saved never shows the bar, even with a different password (so a page cannot use the bar to test guesses against the saved one): change it on `browsecraft://passwords`, or through the site's change-password form. `browsecraft://passwords` lists, shows, copies (cleared from the clipboard after 30 seconds), adds and deletes logins. A shown password is hidden again the next time the page is drawn. If another program (KeePassXC, say) saves the vault while BrowseCraft has it unlocked, BrowseCraft refuses to save over those changes: lock and unlock the vault to load them.

## Settings (`browsecraft://flags`)

`browsecraft://flags` is an engine-rendered settings page with switches and segmented controls. It sets the theme (one of eight, or Follow system; Late Night by default), whether the desktop's own title bar is used, where the tabs are (top, left, right or hidden; top by default), when background tabs are unloaded and how many stay in memory, whether tabs are reopened at start-up, whether the bookmarks bar shows, the tab hover cards (on or off, with or without the page picture, and their delay), and the interface renderer (auto, GPU or software). Changes are stored in the [per-user settings folder](../README.md#settings) and apply immediately after a successful save; save errors are shown on the page or in the status notice. Follow system picks Light or Late Night from the desktop's setting; Auto uses the GPU path when one is available. The renderer choice is made at startup, so restart to apply a change to it.

## Ad blocking

BrowseCraft blocks ads and trackers with a small built-in filter list plus every
`.txt` file in a `filters` folder in the working directory or next to the
executable (EasyList / uBlock Origin syntax), up to 32 MB per file. Element
hiding rules whose selector contains `/*`, `*/` or an unclosed quote are
skipped, since they would break the rules after them. A blocked page shows a "This page
was blocked" notice while the address bar keeps the original address, and a
blocked pop-up shows a short notice under the page.

## Keyboard and mouse

| Shortcut                              | Action                    |
| ------------------------------------- | ------------------------- |
| `Ctrl+L` / `F6` / `Ctrl+S`            | Focus address bar and select its text |
| `Ctrl+T`                              | New tab                   |
| `Ctrl+N`                              | New window                |
| `Ctrl+Shift+\`                        | Show all tabs (overview of every window) |
| `Ctrl+W`                              | Close tab                 |
| `Ctrl+Tab` / `Ctrl+Shift+Tab`, `Ctrl+PageDown` / `Ctrl+PageUp` | Next / previous tab |
| `Ctrl+1` … `Ctrl+8`                   | Go to that tab            |
| `Ctrl+9`                              | Go to the last tab        |
| `Ctrl+Shift+T`                        | Reopen the last closed tab |
| `Ctrl+Shift+N`                        | New tab in a temporary container |
| `Ctrl+F`                              | Find in page              |
| `Ctrl+P` / `F1`                       | Command palette           |
| `Ctrl+Shift+P`                        | Print                     |
| `F12` / `Ctrl+Shift+I`                | Developer tools           |
| `Ctrl+Shift+J`                        | JavaScript console (Automation panel) |
| `F5` / `Ctrl+R`                       | Reload                    |
| `Ctrl+Shift+R`                        | Reload, bypassing the cache |
| `Ctrl+Shift+Delete`                   | Clear browsing data (press twice) |
| `Ctrl+,`                              | Settings (`browsecraft://flags`) |
| `Esc` (page focused, while loading)   | Stop loading              |
| `Alt+Left` / `Alt+Right`, mouse back/forward buttons | Back / forward |
| `Ctrl+Left` / `Ctrl+Right`            | Back / forward on engine pages, when not typing in a field (in web pages they move by a word) |
| `Backspace` / `Shift+Backspace` (page focused) | Back / forward   |
| `Alt+Home`                            | Home (Startpage)          |
| `Ctrl+Plus` / `Ctrl+Minus` / `Ctrl+0`, `Ctrl`+wheel | Zoom in / out / reset |
| `Ctrl+U`                              | View page source          |
| `Ctrl+H`                              | Open the history page (`browsecraft://history`) |
| `Ctrl+D`                              | Bookmark the page, or remove its bookmark |
| `Ctrl+Shift+B`                        | Show / hide the bookmarks bar |
| `Ctrl+Shift+O`                        | Open the bookmarks page (`browsecraft://bookmarks`) |
| `F11`                                 | Make the window fullscreen, or leave fullscreen |
| `Esc` (page in fullscreen)            | Leave page fullscreen     |
| `Ctrl+Shift+S`                        | Save a PNG of the visible page to your Pictures folder (UTC-stamped `browsecraft-YYYYMMDD-HHMMSS.png`); the result shows in a strip under the page |
| `Shift+K` (page focused)              | Show / hide the shortcut cheatsheet (`Esc`, `Enter`, `Space` or `K` closes it) |
| `Space` / `Shift+Space`, `Page Up/Down`, arrows, `Home`/`End` | Scroll |
| `Shift`+wheel                         | Scroll sideways           |
| `Ctrl+Q`                              | Quit (every window)       |

`Ctrl+Shift+S` grabs the page from the screen (Windows, and Linux under X11). The palette's *Screenshot of the whole page* renders the whole document with WebKit instead (Linux), whatever covers the window.

## Forms

These apply to forms on engine pages. Click a text field to type in it. `Enter` submits the form (or adds a line in a `<textarea>`), `Tab` / `Shift+Tab` move between fields and `Esc` leaves the field. Checkboxes and radio buttons toggle on click. A `<select>` moves to its next option on each click, since there is no dropdown. Reset buttons clear the form. Forms submit with GET or POST.

## Links and new windows

Links with `target="_blank"` and pop-ups from WebView2 pages open in the current tab, but only when you opened them (a click, not a script running on its own), only to web pages, and at most one a second. A tab keeps its last 100 Back and Forward entries. On engine pages, `javascript:`, `mailto:` and `tel:` links are ignored. BrowseCraft has a single window; new pages open in tabs.

BrowseCraft does not hand links to other apps: a page (or a frame in it) that opens a `mailto:` link or an app's own scheme gets a "Blocked a link that opens another app" notice instead of the system prompt. A page cannot close its tab with `window.close()`.
