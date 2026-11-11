# ArtCraft Launcher integration

Research of `storytold/craft-launcher` at `c636281` (2026-11-10, release
v0.19.0), done read-only on 2026-11-11. **[V]** = verified in the launcher's
code or on GitHub, **[I]** = inferred. Nothing was pushed, commented or
opened on the launcher repository.

Status for this fork: `--version`, the X11 window class
`io.github.matteomicheletti.browsecraft`, the desktop entry and the
CC-BY-4.0 icon (`assets/app-icon/hicolor/128x128/apps/…png`) are in place
(M1). AppImage, release script and the launcher PR are milestone M7.

## 0. TL;DR

- **[V] The catalog is compiled in.** It is a Rust `const SUITE: [CraftApp; 19]` in `crates/core/src/catalog.rs:150-287`. Icons come from `include_bytes!` (`catalog.rs:332-356`). There is no remote catalog JSON. The remote "feed" (`api.storyteller.ai`) only supplies release info for apps that are already in `SUITE`. It cannot add apps, and it cannot point an app outside `storytold/` (`crates/core/src/feed.rs:146-158`).
- **[V] #989 (community apps as data) is OPEN** and labelled `priority: low` and `backlog`. Nothing from it is implemented: no `craft-app.json` exists anywhere in the code. The two community apps, SolidCraft (#963) and MailCraft (#1360, merged 2026-11-10, shipped in v0.19.0), were each added as a hardcoded row in a PR. A maintainer had to approve each one, and each approval was recorded in AGENTS.md.
- **[V] Draft PR #1115** would load a signed hosted TOML catalog. Even with it, "the compiled community allowlist remains SolidCraft and MailCraft" and "new community apps … still need maintainer review and a launcher release" (quoted from the PR body).
- **So, to get BrowseCraft into the official launcher:** open a PR modelled on #1360. Only a maintainer can approve it, and agents are forbidden from landing third-party apps (`AGENTS.md:129-134`). The other route is to run it in our own fork of the launcher.
- **What we must publish:** GitHub Releases on `matteomicheletti/browser-craft` with semver tags (`v0.2.0`), assets named `browsecraft-<ver>-<os>-<arch>.<ext>`, and a `SHA256SUMS.txt` in `sha256sum` format.

---

## 1. App catalog

### Where entries are defined [V]
- `crates/core/src/catalog.rs:16-54` defines `struct CraftApp`. `:121-147` is the `const fn app(...)` helper, which fills in official defaults. `:150-287` is `SUITE`.
- The doc comment at `catalog.rs:3` says *"The list is data so a later version can fetch it from an API; SUITE is the built-in copy"*. That has not happened yet.
- `CraftApp::by_id` (`:116`) is the only lookup.
- Adding an app touches more than `catalog.rs`. The other compiled-in tables are:
  - `crates/core/src/details.rs` (`AppDetails` page per app; the test `every_app_has_a_page` enforces it)
  - one tagline row per app id in every `crates/ui-egui/src/i18n/*.tsv` (12 files; e.g. `de.tsv:199`: `mailcraft\tMail, calendar and contacts\tE-Mail, Kalender und Kontakte`)
  - `crates/native/src/desktop_entry.rs:20-41` (`meta()`: Linux menu categories and StartupWMClass per id; the fallback is `"Graphics;"` and the class defaults to the bundle id)
  - `app_icon_png` (`catalog.rs:332`)
  - `README.md` (table row, `README.md:287`)
  - `ATTRIBUTION.md` (icon row)
  - `AGENTS.md` (approved community source list, `:110-123`)
  - the tests in `catalog.rs:358+`; `suite_has_nineteen_apps_with_unique_ids` hardcodes the count

### Entry fields (`catalog.rs:16-54`) [V]

| field | meaning |
|---|---|
| `id` | Machine name. It is also the **asset stem, binary name, package name and repo name** (`:19`). Test `:381`: `name.to_lowercase().replace(' ', "-") == id`. |
| `name` | Display name. Also the macOS `.app` stem and the Windows MSI `DisplayName`/folder. |
| `tagline` | Short English text, translated through the tsv files by app id. |
| `color`, `ink_light`, `ink_dark` | `[u8;3]` sRGB: surface colour, text colour on light, text colour on dark. |
| `repo` | `owner/repo` on GitHub. For a community app it is pinned and the feed cannot move it. |
| `former_ids`, `former_names` | Old asset stems and old display names. |
| `mac_bundle_name` | `.app` stem when it differs from `name`. |
| `kind` | `AppKind::Official` or `AppKind::Community`. Test: a community repo must not start with `storytold/`. The UI shows a "Community" badge, and a setting can hide community apps (#1136). |
| `publisher` | Byline on the card. |
| `msi_publisher` | Windows MSI `Publisher` string used in detection (`detect.rs:102-104`). Default `"Learning Machines LLC"`. |
| `bundle` | Bundle id. Default `ai.storyteller.<id>`. Used as the Flatpak id, the Linux `.desktop` id, the icon name, the macOS bundle id, and the Arch package name for "release" pkgs. |
| `former_bundles` | Old bundle ids. |
| `site` | Website. Default `getartcraft.com/apps/<id>`. |
| `mac_team` | Apple Developer ID team. `None` means the DMG is never installed, only opened if already present (`catalog.rs:76-78`, `install.rs:200-204`). |

- **Not in the schema [V]:** asset regex, executable path, install dir and launch args. These are all derived by convention from `id`, `name` and `bundle` (see §2).
- **Per-user launch flags:** the user sets these in a dialog ("launch options", `dialogs.rs:240`). They are not set in the catalog.

---

## 2. Releases, assets, checksum, install, update, uninstall, launch

### Fetching [V]
1. **Feed.** `GET https://api.storyteller.ai/v1/craft_apps/release_info` (`feed.rs:25`).
   - It is used only if the app's entry is present and less than 24 h old (`feed.rs:161-185`). Otherwise the answer is "AskGitHub".
   - [I] storyteller-web probably only polls official/known repos, so a new community app would go straight to GitHub.
2. **GitHub API.** `api.github.com/repos/{repo}/releases/latest`, or `/releases?per_page=30` when prereleases are wanted. Requests use ETags (`crates/native/src/source.rs:120-130`).
3. **Quota-free fallback.** The launcher follows the redirect of `https://github.com/{repo}/releases/latest` to get the tag, then downloads `SHA256SUMS.txt`. **The asset list is then built only from SHA256SUMS.txt** (`release.rs:263-282`, `source.rs:181`).
   - So SHA256SUMS.txt is effectively mandatory. Without it the app is invisible when the API is rate-limited or blocked.
- **Download URLs** must be exactly `https://github.com/{repo}/releases/download/{tag}/{name}` (`release.rs:113-147, 243`).
- **Allowed characters:** file names and tags may use only `[A-Za-z0-9._+-]`. A tag can be at most 64 characters, and neither may start with a dot (`release.rs:290-316`).
- **Drafts** are skipped (`release.rs:174`).

### Version detection from releases [V]
- The tag is parsed as semver: `v0.2.0` and `0.2.0` work, and so does a two-part `0.2` (`version.rs:11-34`).
- **Stable track:** non-prerelease, stable semver, highest version (`release.rs:194-198`).
- **Nightly track:** GitHub prerelease, with a pre-release identifier containing `nightly` (e.g. `v0.3.0-nightly.20261111`), and it must publish a checksum (`release.rs:182-199`).
- **Pinned:** exact tag.
- `bind_artifact_version` (`install.rs:313-342`) refuses the install if the version in the asset filename differs from the tag's version. So **the asset name must contain the same version as the tag**.

### Asset naming and picking [V] (`crates/core/src/platform.rs`)
- **Format:** `<stem>-<version>-<os>-<arch>[-portable].<ext>` (`:3-4`, parser `:219-264`). The stem match is case-insensitive.
- A Tauri-style `<Stem>_<ver>_<arch>.<ext>` is also accepted, for DMG/MSI/ZIP only (`:242-261`).
- **Platform tokens** (`:145-162`), best first:
  - Linux x64: `linux-x86_64`. Linux arm64: `linux-aarch64`.
  - Windows x64: `windows-x64`, then `windows-x86`. Windows arm64: `windows-arm64`, then x64, then x86.
  - macOS: `macos-universal`, then `macos-aarch64`/`macos-arm64`, then `macos-x86_64`.
- **Extensions** (`:85-97`): `.msi`, `-portable.zip`, `.dmg`, `.AppImage` (case matters in the parser), `.deb`, `.rpm`, `.pkg.tar.zst`, `.flatpak`, `.tar.gz`.
  - Anything else is ignored, e.g. `.AppImage.zsync`, `SHA256SUMS.txt`, `*-cli-*`, `*-web-*`.
- `pick_asset` (`:268-278`) tries each package kind in preference order and, within a kind, each token. If a kind has no asset it falls through to the next kind.

### Which kind on Linux [V] (`install.rs:1187-1240`, `choose_kinds_with` `:588-628`)
- **Launcher running as an AppImage** (`$APPIMAGE` set): the order is AppImage, then the distro package, then Flatpak.
- **Launcher installed natively on Manjaro:**
  - `.pkg.tar.zst` comes first, but only if pkexec and pacman are present.
  - Then `.flatpak` if flatpak is installed, then `.AppImage`.
  - `.tar.gz` is used only inside the Flatpak-sandboxed launcher.
- **Consequence:** if we publish only an AppImage, Manjaro gets the AppImage. If we also publish `.pkg.tar.zst`, it would be preferred. The launcher treats it as "its own" (updatable) only if the pacman package name equals the bundle id (`install.rs:643-645`). Otherwise it is "managed elsewhere".
- **Updates keep the original kind:** an existing install is updated with the same kind it was installed with (`:597-620`).

### SHA-256 verification [V] (`install.rs:661-698`)
- **Order of preference:**
  1. The asset's GitHub API `digest` field (`"sha256:<hex>"`, `release.rs:245`). GitHub fills this in automatically for uploaded assets.
  2. The feed's hash.
  3. The line in `SHA256SUMS.txt` (exact name, parsed as `sha256sum` output `<hex>  <name>` or `<hex> *<name>`, `release.rs:301-310`).
- **No checksum means a refused install:** *"the release publishes no checksum for …"*.
- A per-file `.sha256` sidecar is **not** used.
- Hashes come from the release, never from the catalog.

### Install (Linux AppImage) [V] (`install.rs:1451-1490`, `integrate` `:1599-1650`, `desktop_entry.rs`)
- **Placement:**
  - The file is copied to `~/Applications/<asset name unchanged>` (or a user-chosen folder), set to `chmod 0755`, then atomically renamed into place.
  - The old AppImage recorded in the manifest is deleted.
  - The launcher refuses to overwrite a file it didn't install unless the user confirms.
- **Menu entry:** writes `~/.local/share/applications/<bundle_id>.desktop` with:
  - `Name`, `Comment=tagline`, `Exec="<path>" %F`, `Icon=<bundle_id>`
  - `Categories` from `meta()`
  - `StartupWMClass=<bundle_id>` (or the per-app override), plus `X-ArtCraft-Launcher=true`
- **Icon:** writes the bundled 128 px PNG to `~/.local/share/icons/hicolor/128x128/apps/<bundle_id>.png`.
- **Not done:** no extraction and no AppImage mounting. A running app is never replaced ("X is running. Quit it"), and downgrades are blocked unless the user explicitly picks an older version.
- **Other Linux kinds:**
  - `.deb`, `.rpm`, `.pkg.tar.zst` and `.flatpak` go through the system tools behind a pkexec prompt (`install.rs:1243+`).
  - `.tar.gz` (sandbox only) must contain `bin/<id>` plus `share/…`. It is unpacked to `<launcher data>/apps/<id>` with `HOME` redirected (`install.rs:1492-1560`).

### Installed-version detection [V] (`crates/native/src/detect.rs`)
1. **Launcher manifest record**, with the version stored at install time (`:55-73`).
2. **Linux AppImages** found in `~/Applications`, `~/AppImages` (Gear Lever) or `~/.local/bin`, by **filename**: either `browsecraft-0.2.0-linux-x86_64.AppImage` or a bare `browsecraft.AppImage` with unknown version (`:76-92, 553-600`).
3. **deb/rpm/pacman/flatpak** package lists, matched by id or bundle id.
4. **A binary named `<id>`** on PATH, `/usr/bin`, `/usr/local/bin` or `~/.local/bin`. The launcher runs `<binary> --version` and parses `"<id|name> <semver>"` (`:136-152`, `:714-717`).
- **[V] BrowseCraft:** I found no `--version` handling in `src/`. Add one that prints `browsecraft 0.2.0`. It is only needed for case 4.

### Launch [V] (`install.rs:844-905`, `process.rs:347-364`)
- The binary or AppImage is spawned directly, with the user's launch flags and its directory as cwd.
- The env var from `craft_update::pins::PATH_ENV` points at the shared version-pin file.
- A Flatpak app is started with `flatpak run <bundle id>`, and a macOS app with `open`.

### Uninstall [V] (`install.rs:776-840`)
- **AppImage:**
  - Deleted only if it sits in `~/Applications` or is the recorded path.
  - The launcher's `.desktop` and icon files are removed too.
  - AppImages in `~/AppImages` or `~/.local/bin` are treated as the user's own and left alone.
- **Packages:** removed through the package manager behind pkexec.

### Windows / macOS [V]
- **Windows MSI:**
  - Detected through the Uninstall registry entry: `Publisher == msi_publisher` and `DisplayName == name`.
  - The exe is `<InstallLocation or Program Files\<name>>\<id>.exe` (`detect.rs:102-118`).
  - The default install is per-user into `%LOCALAPPDATA%\Programs\…`.
- **Windows portable zip:** must contain `<id>.exe` (`portable.rs:307`).
- **macOS:**
  - A DMG holding `<name>.app`.
  - Installed **only** if `mac_team` is set and the app is signed with that Developer ID team. Otherwise the app is only opened if already present (MailCraft today).

---

## 3. Community / third-party apps

- **#989 "Unofficial Craft-Style Apps … as Data"** (by YalKouassi, 2026-10-29): OPEN, labels `priority: low`, `install`, `backlog`.
  - **Proposal:** a `craft-app.json` descriptor per release, a Settings ▸ Unofficial Apps ▸ Add screen, zero liability for the launcher, signer pinning, and moving identity into `craft_update::App`.
  - **Comments (3):**
    - ticket-craft bot: related #309, #656.
    - Mansi1: +1, asking for at least a list of links.
    - The author is trialling it in his own fork: https://github.com/YalKouassi/craft-unofficial-launcher.
  - **No maintainer response.**
- **[V] Not implemented.**
  - `grep craft-app.json` finds nothing.
  - `SUITE` is still a const array.
  - Related issues are all still OPEN: #309 (customizable library), #589 (publish SUITE as data), #619 (third-party updates PR), #656 (configurable catalog for forks).
  - #384 (opt-in community GitHub sources in Settings) was CLOSED unmerged.
- **[V] Existing community apps:**
  - **SolidCraft:** `tangency-labs/solidcraft`. Merged in PR #963 on 2026-10-28; macOS signing was added in #1058.
  - **MailCraft:** `UCS-Care-Group/mailcraft`. Merged in PR #1360 on 2026-11-10.
- **Pending, unmerged community PRs:**
  - NoteCraft #979 (CHANGES_REQUESTED)
  - DrawCraft #1326
  - ASUSCraft #1159
  - MusicCraft docs #1336
- **[V] Reference PR: #1360 (MailCraft).** Files changed:
  - `crates/core/src/catalog.rs` (+33: SUITE row, `app_icon_png` arm, count test 18→19, a specific test `mailcraft_is_a_community_app_not_installed_on_mac`)
  - `crates/core/src/details.rs` (+19, text-only details page, `shots: &[]`)
  - `assets/app-icons/mailcraft.png`, plus `LICENSE-MIT-MailCraft.txt` and `LICENSE-APACHE-MailCraft.txt` next to it
  - the tagline row in all 12 `i18n/*.tsv`
  - `ATTRIBUTION.md` row, `README.md` table row
  - `AGENTS.md`, adding the repo to the approved sources with an approval date
  - Approval came from maintainer `skylartaylor` in a PR comment, after checking the macOS DMG signature.
  - The PR body has Summary, Evidence (before/after tests, `cargo xtask ci`, built and launched on Linux) and Merge Danger (door/blast radius).

---

## 4. Icons

### How they're used [V]
- Each icon is a **128×128 RGBA PNG**, compiled in with `include_bytes!` from `assets/app-icons/<id>.png` (`catalog.rs:330-356`). The test `every_app_has_its_icon` checks the PNG magic bytes.
- It is drawn as the image itself, with no mask or background added by the launcher:
  - **Grid card:** 56 px, with a soft drop shadow of corner radius 12, and it lifts on hover (`crates/ui-egui/src/views.rs:621-636`).
  - **Table row:** 38 px (`views.rs:885-888`).
  - **Dialog and details headers** (`dialogs.rs:123,204,240,277`, `details.rs:362`).
- **Apps that aren't installed are shown desaturated to grey** (`icons.rs:13-46`). So the icon must still read well in greyscale.
- **Rules:**
  - The same PNG is installed as the Linux menu icon.
  - `AGENTS.md:169-170`: `assets/app-icons/` is "copied verbatim from each app's repo (`assets/app-icon/hicolor/128x128/apps/<bundle>.png`); refresh it from there, never redraw".
  - It needs an `ATTRIBUTION.md` row, and its licence must be on the allowlist (`AGENTS.md:34-49`: MIT, Apache-2.0, BSD, ISC, CC0, CC-BY-4.0, CC-BY-SA-4.0, OFL…).
- **[V] Licence problem for BrowseCraft:** BrowseCraft is GPL-3.0-or-later (`Cargo.toml`), and **GPL is not on the allowlist**. The icon file must be licensed separately (e.g. CC-BY-4.0 or MIT OR Apache-2.0), with that licence file in our repo next to the icon. The existing `assets/logo.png` is 1254×1254, with no 128 px hicolor export.

### Visual style [V, from the actual files]


- **Shape:**
  - The official icons are a **full-bleed rounded square (squircle)** filling the whole 128 px canvas, with no outer padding.
  - Corners are transparent. The corner radius is about 26–28 px (≈21% of the side; the first opaque pixel on the diagonal is at about 8,8).
- **Background:** a **single flat, saturated brand colour** equal to `CraftApp::color`, e.g. PhotoCraft `#2f7bf5`, GridCraft green, DeckCraft orange. There is no gradient.
- **Motif, family A:**
  - The "hero" apps (Photo, Light, Film, Design, Vector, Pdf, Effect, AniCraft, Fig, Texture, Material, Craft Anything) use a **black-and-white engraving/woodcut-style animal**: fox, lynx, owl, cat, dragon, lion, unicorn, rabbit, chameleon, zebra, rhino, raven.
  - The animal is shown as a bust, large, with dense cross-hatching. It often bleeds off the bottom or side edges.
  - It sits on an off-white/cream fill (~`#efe9dc`), with black ink lines.
- **Motif, family B:**
  - The office/utility apps (WordCraft, DeckCraft, GridCraft, CADCraft, SoundCraft) use a **simple flat pictogram**: document, easel with chart, spreadsheet, set square, stylised bird.
  - The pictogram is drawn in cream with thick black outlines, centred with roughly 15–20% padding, on the flat colour square.
  - Several of these are labelled "placeholder" in `ATTRIBUTION.md`.
- **Outliers (community):**
  - MailCraft: glossy blue gradient tile with a 3D envelope.
  - SolidCraft: no tile; a transparent isometric blue 3D block.
  - So community apps are not forced into the house style. [I] Matching family A or B would still look most "native".
- **Representative icons:**
  - `photocraft.png`, `lightcraft.png`, `figcraft.png` (engraving family)
  - `wordcraft.png`, `gridcraft.png`, `soundcraft.png` (pictogram family)
  - `mailcraft.png`, `solidcraft.png` (community outliers)

---

## 5. Rules and licence
- **Launcher licence:** `MIT OR Apache-2.0` (`Cargo.toml:8`, `LICENSE-MIT`, `LICENSE-APACHE`). The brand marks in `docs/brand/` are trademark-only (`LICENSE-brand.txt`).
- There is no CONTRIBUTING.md. `CLAUDE.md` is just `@AGENTS.md`. The relevant rules in `AGENTS.md` are:
  - **Agents never land or approve a third-party app** (`:129-134`). Only the maintainer approves, and approval is recorded in AGENTS.md with a date.
  - Downloads only from approved repos; SHA256SUMS verification; macOS only with the app's own Developer ID team (`:108-123`).
  - Every bundled asset gets an `ATTRIBUTION.md` row (path, title, author, source, licence); the licence must be on the allowlist (`:34-49`, `:163`).
  - Every user-facing string goes through i18n, so the tagline row is needed in all `complete_menus` tsv files (`:151-156`).
  - `cargo xtask ci` must pass (fmt, clippy -D warnings, tests, wasm).
  - House rules: comment on an issue first and claim it (rule 2); **no agent attribution in the PR** (rule 15); no CI changes in a feature PR.
  - Rust only (no Tauri/Electron) applies to the launcher and to native Craft apps.
  - [I] Before a maintainer approves, they may look at the repo owner, age and history, and whether the builds are signed (`:132-133`). BrowseCraft is a fork of `freewright-dev/BrowseCraft`, so expect the question "why the fork rather than upstream?".

---

## 6. Proposed catalog entry for BrowseCraft [I, built from the verified schema]

**Id choice.** The id must equal `name.to_lowercase().replace(' ', "-")`, and it is also the asset stem and binary name. The Cargo package/binary is already `browsecraft`, so use **id `browsecraft`, name `BrowseCraft`**.
- `browser-craft` would force the display name "Browser Craft", the binary `browser-craft`, and assets `browser-craft-…`.
- The repo name can stay `browser-craft`; it is independent of the id.

```rust
// crates/core/src/catalog.rs, in SUITE after mailcraft (bump [CraftApp; 19] -> 20 and the count test)
CraftApp {
    kind: AppKind::Community,
    publisher: "Matteo Micheletti",                       // card byline (or an org name)
    msi_publisher: Some("BrowseCraft contributors"),      // must equal the MSI's Publisher
    bundle: Some("io.github.matteomicheletti.browsecraft"),// also the Wayland app_id / .desktop id / Flatpak id
    site: Some("https://github.com/matteomicheletti/browser-craft"),
    // No Developer ID signing yet: macOS copies are opened, never installed.
    mac_team: None,
    ..app(
        "browsecraft",
        "BrowseCraft",
        "Web browsing",                                    // + row "browsecraft\tWeb browsing\t<translation>" in all 12 tsv
        [[0x4b, 0x3f, 0xd1], [0x3a, 0x30, 0xa8], [0xa9, 0xa1, 0xff]], // icon tile #4B3FD1, ink_light, ink_dark
        "matteomicheletti/browser-craft",
    )
},
```

Also needed in the same PR:
- **`catalog.rs`:** `"browsecraft" => include_bytes!("../../../assets/app-icons/browsecraft.png")` in `app_icon_png`, plus a `browsecraft_is_a_community_app…` test.
- **`details.rs`:** an `AppDetails { id: "browsecraft", headline, about, features, shots: &[], platforms: PLATFORMS }` entry.
- **`desktop_entry.rs` `meta()`:** `"browsecraft" => ("Network;WebBrowser;", None)`. The default would be `Graphics;`.
- **Repo files:**
  - `assets/app-icons/browsecraft.png`, 128×128, copied verbatim from our repo's `assets/app-icon/hicolor/128x128/apps/io.github.matteomicheletti.browsecraft.png`
  - the icon licence file(s)
  - `ATTRIBUTION.md` row, `README.md` row, `AGENTS.md` approved-source line (the maintainer decides)

**Requirement on the browser itself [I, from `desktop_entry.rs` header]:**
- Set the window app_id/WM_CLASS to `io.github.matteomicheletti.browsecraft` so the GNOME dock matches the launcher's `.desktop`.
- Add `--version` printing `browsecraft <semver>`.

---

## 7. Exact release assets to publish (repo `matteomicheletti/browser-craft`)

**Tag and release settings:**
- Tag `v0.2.0`, a published release (not a draft), not marked prerelease.
- Every asset's version must equal the tag's version.

**Linux x86_64 (first target):**
```
browsecraft-0.2.0-linux-x86_64.AppImage         # required; installed to ~/Applications as-is, chmod 755
browsecraft-0.2.0-linux-x86_64.AppImage.zsync   # optional, ignored by the launcher
browsecraft-0.2.0-linux-x86_64.tar.gz           # optional: bin/browsecraft + share/... (Flatpak-sandboxed launcher only)
SHA256SUMS.txt                                  # required in practice
```

**Format of `SHA256SUMS.txt`:**
- Generate it with `sha256sum browsecraft-0.2.0-* > SHA256SUMS.txt` in the asset directory, so the names are bare and match the assets exactly.
- Each line is `<64 hex>  <file name>`, one line per asset.
- Do not publish only per-file `.sha256` sidecars; the launcher does not read them.
- Don't publish a `.pkg.tar.zst` unless its pacman package name equals the bundle id. Otherwise the launcher prefers it on Manjaro but treats it as "managed elsewhere".

**Later, Windows:**
```
browsecraft-0.2.0-windows-x64.msi            # MSI: DisplayName "BrowseCraft", Publisher == msi_publisher, installs browsecraft.exe
browsecraft-0.2.0-windows-x64-portable.zip   # browsecraft.exe inside
browsecraft-0.2.0-windows-arm64.msi          # optional
```

**Later, macOS** (installed only if Developer ID signed and `mac_team` set; otherwise only opened):
```
browsecraft-0.2.0-macos-universal.dmg        # contains BrowseCraft.app; or -macos-aarch64 / -macos-x86_64
```

All of the above go into the same `SHA256SUMS.txt`.

**Nightlies (optional):** a GitHub prerelease tagged e.g. `v0.3.0-nightly.20261111`, with its own `SHA256SUMS.txt`.

---

## 8. Open questions / caveats
- **[I] Feed coverage:** the storyteller feed probably won't list browsecraft, so every check hits the GitHub API (60 requests/h unauthenticated), with the SHA256SUMS fallback when that runs out.
- **[V] Fork route:** without maintainer approval, the only option is a fork of the launcher, e.g. YalKouassi/craft-unofficial-launcher, or our own fork with one extra SUITE row. The license (MIT/Apache) allows it, but the ArtCraft brand marks in `docs/brand/` may not be reused.
- **[V] Draft PR #1115** (signed hosted TOML catalog, updated 2026-11-11) may change the catalog's source. It keeps community apps compiled-in and pinned, so the row above would still be the way in.
