# Importing bookmarks and history

`browsecraft://import` (palette: *Import bookmarks and history…*) or `browsecraft import` brings in **bookmarks and history only**. Passwords, cookies, open tabs and extensions are not imported. Passwords may come later: Firefox encrypts them with NSS and Chrome with the desktop keyring.

## Sources

| Browser | Where profiles are found (Linux) | Read |
|---|---|---|
| Firefox, LibreWolf, Waterfox | `profiles.ini` in `~/.mozilla/firefox`, `~/.var/app/org.mozilla.firefox/.mozilla/firefox` (Flatpak), `~/snap/firefox/common/.mozilla/firefox` (Snap), `~/.librewolf`, `~/.waterfox` | `places.sqlite` (`moz_bookmarks`, `moz_places`, `moz_historyvisits`) |
| Chrome (+ Beta, Dev), Chromium, Brave, Edge, Vivaldi, Opera | `~/.config/{google-chrome, chromium, BraveSoftware/Brave-Browser, microsoft-edge, vivaldi, opera}` (also `$XDG_CONFIG_HOME`), Flatpak `~/.var/app/<id>/config/...`, Snap Chromium | `Bookmarks` (JSON), `History` (`urls`, `visits`); profile names from `Local State` |
| Any browser | an exported `bookmarks.html` (Netscape format), by path or dropped on the window | bookmarks |

Profiles without a `places.sqlite` / `Bookmarks` / `History` are not listed.

## Mapping

- Firefox: toolbar → bar; *Bookmarks Menu*, *Other Bookmarks*, *Mobile* → folders of those names. Tags, separators and `place:` queries are skipped. Times are microseconds since 1970.
- Chromium: `bookmark_bar` → bar, `other` → Other bookmarks, `synced` → *Mobile*. Times are microseconds since 1601-01-01 UTC (minus 11 644 473 600 s).
- HTML: the folder marked `PERSONAL_TOOLBAR_FOLDER` → bar, the rest → Other bookmarks.
- Destination: by default everything goes into *Other bookmarks › Imported from <browser> (<profile>)* (the other bar as a subfolder); *merge* puts the other bar into ours and the rest into Other bookmarks.
- Only `http(s)` bookmarks are kept. History skips hidden entries, sub-frames and embeds; typed visits stay typed.
- Dedup: the same address in the same folder, the same (address, time) visit. Folders with the same name are merged, so importing twice adds nothing.
- Range: all, or the last N days, never older than the history retention setting (default one year).

## Safety

- The other browser's files are never written: SQLite databases are copied with their `-wal`/`-journal` into a private temporary folder (`$TMPDIR`) and the copy is opened read-only; the copy is deleted afterwards. This works while that browser runs. `Bookmarks` is read directly.
- Limits: databases 8 GB, Bookmarks JSON and HTML 64 MB, 2 million visits (newest first), 50 000 bookmarks, 10 000 folders, 32 levels; HTML parsing is a linear tag scan.
- Before an import `bookmarks.json.import-backup` and `history.sqlite.bak` are written. *Undo last import* (import page, bookmarks page, `--undo`) removes what the last import added (imported items are tagged), keeping later changes.
- A dry run only counts.

## Command line

```
browsecraft import --list
browsecraft import firefox|librewolf|waterfox|chrome|chrome-beta|chrome-dev|chromium|brave|edge|vivaldi|opera
    [--profile NAME|DIR|PATH] [--bookmarks] [--history] [--days N] [--merge] [--dry-run]
browsecraft import html --file bookmarks.html [--merge] [--dry-run]
browsecraft import --export [DIR]      # bookmarks-<date>.html, default ~/Downloads
browsecraft import --undo
```

Prints a JSON report (found, added, already there, skipped, errors) on stdout, progress on stderr; exit 1 on errors, 2 on usage. Both parts run even if one fails.

It works while the browser is open: history is SQLite (WAL, busy timeout); `bookmarks.json` is changed under `bookmarks.lock`, and the running browser checks the file every 2 s and reloads it (bar, pages, palette).
