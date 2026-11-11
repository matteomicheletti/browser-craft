# Command palette and screen recording

The palette is a spotlight-style overlay (like VS Code or Raycast) that
reaches every browser function. Open it with **Ctrl+P** (Cmd+P on macOS) or
**F1**. Printing stays on Ctrl+Shift+P and is also a palette command.

## Using it

Type to search. Matching is fuzzy: `scr` finds "Screenshot", `dmt` finds
"Toggle dark mode"; word starts and prefixes rank higher and the matched
letters are highlighted. Several words must all match (`dark site`). Commands
you ran recently are boosted, and with an empty box they are listed first.

| Key              | Action                                   |
| ---------------- | ---------------------------------------- |
| Up / Down        | Move the selection (wraps)               |
| PageUp / PageDown| Move by six rows                         |
| Tab / Shift+Tab  | Jump to the next / previous section      |
| Enter or click   | Run the selected row                     |
| Esc, click outside | Close                                  |

Prefixes narrow the search:

| Prefix | Searches                              |
| ------ | ------------------------------------- |
| `>`    | commands only                         |
| `@`    | open tabs, then tab groups (picking a group expands it and goes to it) |
| `#`    | bookmarks and history                 |
| `~`    | containers (`~ example.com` opens the address in the chosen one) |
<<<<<<< HEAD
| `\|`   | open tabs to show beside the current one (split view) |
| `&`    | tab groups to add the current tab to, and "New group" |
=======
| `%`    | themes (*Change theme…* opens this list; the current one is tagged) |
>>>>>>> wave3/chrome
| `?`    | lists the prefixes                    |

Text that looks like an address (`example.com`, `localhost:3000`) or a search
gets an "Open ..." / "Search the web for ..." row first, using the same rules
as the address bar (`address::resolve`). Commands that need a web page (print,
screenshot, find, zoom, ...) are greyed out on internal pages. "New tab in
container..." switches the palette to the `~` list, "Split view with tab..."
to `|` and "Add tab to group..." to `&`. "Start screen recording"
turns into "Stop screen recording" while one is running.

The recent-commands list is `palette-recent.json` in the config folder (a
list of command keys, at most 8).

## Wiring it into the browser

Done in `src/browser/features.rs` (`toggle_palette`, `palette_sources`,
`run_command`); the palette counts as an open popup, so the page's native
window is hidden behind a WebKit snapshot while it shows.

```rust
let mut palette = Palette::new();           // once
// Ctrl+P / F1:
palette.toggle();                           // or open() / open_with("@ ")
// every frame, with the web view hidden while palette.is_open():
if let Some(out) = palette.ui(ctx, &colors, &sources) {
    match out {
        PaletteOutcome::Run(id) => self.run_command(id),
        PaletteOutcome::SwitchTab(i) => self.switch_to(i),
        PaletteOutcome::OpenUrl(url) => self.open_in_new_tab(url),
        PaletteOutcome::OpenInContainer { url, container } => { /* url empty = blank tab */ }
    }
}
```

`PaletteSources` (`palette.rs`) carries `tabs: Vec<TabItem>`,
`bookmarks`/`history: Vec<LinkItem>` (history most recent first),
`containers: Vec<ContainerItem>`, `on_web_page: bool` and `recording: bool`.
`ui` consumes the arrow, Enter, Tab and Esc keys while open, so the browser's
own shortcut handling should skip them when `is_open()`.

`commands.rs` holds `CommandId`, `Command` and `catalog()`. To add a command:
a variant, a catalog row (the array length is checked by the compiler) and an
arm in the browser's `run_command`. The `key` of a row is persisted; never
rename it. `Print` is implemented with the web view's `print()`.

## Screen recording (`recording.rs`)

`Recorder` drives `ffmpeg` (`x11grab`, H.264 MP4 by default, WebM and GIF
optional) and writes `~/Videos/BrowseCraft-<timestamp>.<ext>`.

```rust
let mut rec = Recorder::new();
let id = recording::x11_window_id(&window).ok_or("not an X11 window")?;
let path = rec.start(&Options::new(Target::Window(id)))?;  // Err if ffmpeg is missing
rec.is_recording();   // polled; reaps an ffmpeg that died
let file = rec.stop()?;  // sends `q`, waits up to 10 s, then kills
```

- The browser window is an X11 window on Linux, an XWayland client under
  Wayland, so grabbing it by window id works in both sessions (checked on
  Wayland/XWayland). The grab reads the window's area of the X screen, so the
  native web view is included, but anything stacked over the window (other
  windows, the palette's own snapshot) is recorded as it is on screen.
- `Target::Region` records a screen rectangle instead.
- GIF is recorded as an MP4 and converted when the recording stops, which can
  take a few seconds (encoding it live loses frames on a static page).
- `stop` blocks for the time ffmpeg needs to close the file; call it from a
  worker thread if that matters.
- Not available on Windows and macOS yet: `start` returns an error.
- Manual check:
  `BC_REC_WINDOW=<xid> DISPLAY=:0 cargo test records_real_window -- --ignored`
  (`BC_REC_FORMAT=webm|gif`). Find the id with `xdotool search --class browsecraft`.

## Tests

`cargo test palette commands recording` covers the fuzzy scorer, prefix
modes, URL detection, recent commands, Enter/Esc/arrow handling through
headless egui frames, and the ffmpeg command lines.
