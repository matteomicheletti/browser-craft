# Packaging and releases (Linux x86_64)

Everything runs locally (no cloud CI on this fork): `scripts/check.sh`,
`scripts/package-appimage.sh`, `scripts/release.sh`.

## Approach: lean AppImage (host WebKitGTK)

Two options were evaluated on Manjaro (webkit2gtk-4.1 2.52.6, glibc 2.44).

| | (a) bundle WebKitGTK | (b) lean, host WebKitGTK (chosen) |
| --- | --- | --- |
| Size | `libwebkit2gtk-4.1.so` alone is 90 MB, `libjavascriptcoregtk` 36 MB, 143 shared libraries in its `ldd` closure, plus GTK, GStreamer, helper processes: roughly 200-300 MB uncompressed, 80-120 MB compressed | **6.6 MB** |
| Helper processes | `WebKitNetworkProcess` / `WebKitWebProcess` and the injected bundle have `/usr/lib/webkit2gtk-4.1` compiled in; they must be binary-patched (what Tauri's linuxdeploy plugin does) | Host paths are correct, nothing to patch |
| bubblewrap sandbox | WebKit binds the helper directory read-only into the sandbox; a path under `/tmp/.mount_*` (FUSE mount) is not visible to the helper's own `bwrap` setup without extra work, and the host `bwrap` and `xdg-dbus-proxy` are still needed | Works unchanged: verified below |
| Portability | Bundled libs need a glibc at least as new as the build host, so it is no more portable than (b) unless built in an old-distro container | Needs the distro packages, which every target distro ships |
| Security updates | Frozen in the image: an unmaintained WebKit is a real risk for a browser | The distro patches WebKitGTK |

Decision: **(b)**. It works reliably including the sandbox, and a browser should
not ship a frozen WebKit. `AppRun` checks for `libwebkit2gtk-4.1.so.0` and
`libgtk-3.so.0` before starting (the binary links GTK dynamically, so without
the check the user would only see a loader error) and shows a zenity/kdialog/
notify-send message plus a stderr message with the install command, exit 127.

## Contents of the AppImage

```
AppRun                                   checks host libraries, then execs usr/bin/browsecraft
usr/bin/browsecraft                      release binary
usr/bin/filters/*.txt                    adblock lists (src/adblock.rs reads filters/ next to current_exe())
usr/share/applications/…desktop          also at the root
usr/share/metainfo/…metainfo.xml         AppStream (version/date filled in by the script)
usr/share/icons/hicolor/{128,256,scalable}
usr/share/LICENSE, THIRD_PARTY_NOTICES.md
```

`current_exe()` resolves to the FUSE mount (`/tmp/.mount_*/usr/bin/browsecraft`),
so `filters/` is found there. The runtime is the pinned type2-runtime (static,
no libfuse2 needed, only `fusermount`); without FUSE use
`--appimage-extract-and-run`.

## Requirements on the host

- x86_64 Linux with glibc >= **2.44**. This is the symbol version the Manjaro
  build links against (`objdump -T`), so **today the AppImage only runs on very
  recent distros** (Arch/Manjaro, Fedora 44+, rolling). To support Ubuntu
  22.04/24.04 and Debian 12 the release must be built in an older-glibc
  container (not done: needs podman/docker plus a Rust toolchain there).
- `webkit2gtk-4.1`, `gtk3`, `bubblewrap`, `xdg-dbus-proxy`
  (Arch/Manjaro: `webkit2gtk-4.1 gtk3 bubblewrap xdg-dbus-proxy`;
  Fedora: `webkit2gtk4.1 gtk3 bubblewrap xdg-dbus-proxy`;
  Ubuntu 22.04+/Debian 12: `libwebkit2gtk-4.1-0 libgtk-3-0 bubblewrap xdg-dbus-proxy`).
- X11 or XWayland (the browser window is an X11 client).

## Build

```sh
scripts/package-appimage.sh            # release build + dist/<version>/browsecraft-<version>-linux-x86_64.AppImage
scripts/package-appimage.sh --no-build # reuse target/release/browsecraft
```

`appimagetool` 1.9.1 and runtime 20251108 are downloaded once into
`~/.cache/browsecraft-packaging` (override with `BROWSECRAFT_PACKAGING_CACHE`)
and checked against SHA-256 values pinned in the script. The AppDir is built in
a temp dir, never in the repo. `.AppImage.zsync` is produced only if
`zsyncmake` is installed (it is not here, so none was produced; the launcher
ignores it anyway).

## Cutting a release

```sh
git switch main                         # clean tree required
scripts/release.sh 0.2.0 --dry-run      # prints every step, changes nothing
scripts/release.sh 0.2.0 [--bump]       # check.sh, AppImage, dist/0.2.0/SHA256SUMS.txt, local tag v0.2.0
scripts/release.sh 0.2.0 --publish      # explicit: pushes the tag and runs gh release create
```

- The version must equal `Cargo.toml`'s; `--bump` edits it (after a prompt) and
  commits `chore(release)`.
- `SHA256SUMS.txt` is `sha256sum` output with bare file names, as the ArtCraft
  Launcher requires. The asset name `browsecraft-<ver>-linux-x86_64.AppImage`
  carries the same version as the tag.
- Without `--publish` the script only creates the local tag and prints the
  `git push` and `gh release create … --repo matteomicheletti/browser-craft`
  commands. `--publish` after a normal run publishes the already built assets.
- `scripts/check.sh`: `cargo build/test --locked`, `cargo clippy` (the repo's
  `unwrap_used`/`expect_used` deny lints fail it; other warnings are existing
  debt, `CLIPPY_STRICT=1` denies them), `cargo deny check` if installed, and
  `rustfmt --check` only on `.rs` files changed against `main`.

## Verification (2026-11-11, Manjaro, GNOME Wayland, this box)

- Artifact: `dist/0.1.0/browsecraft-0.1.0-linux-x86_64.AppImage`, 6,924,792
  bytes (6.6 MB).
- `./browsecraft-0.1.0-linux-x86_64.AppImage --version` and the same under
  `env -i PATH=/usr/bin`: `browsecraft 0.1.0`.
- Launched with temp `XDG_CONFIG_HOME`/`XDG_DATA_HOME` on
  `http://127.0.0.1:8799/features.html` (python `http.server` on `tests/web`):
  window title `probe 17/17 - BrowseCraft`, screenshot shows 17/17 PASS.
- `https://example.com` under `env -i` (only PATH, HOME, display and D-Bus
  variables): title `Example Domain - BrowseCraft`.
- `ps --forest`: `…/.mount_*/usr/bin/browsecraft` has children
  `WebKitNetworkProcess`, `bwrap … xdg-dbus-proxy` and
  `bwrap … /usr/lib/webkit2gtk-4.1/WebKitWebProcess`, so the sandbox is active.
- Missing libraries (simulated by a stub `ldconfig`): message with the package
  list, exit status 127.
- `appstreamcli validate` on the generated metainfo: passed.

## Limitations and later work

- glibc baseline (above). A container build is the fix.
- Needs the distro's WebKitGTK; a bundled variant (a) could be added as a
  second asset, but would not be preferred by the launcher (same name pattern).
- x86_64 only. AArch64 needs a native or cross build plus the
  `appimagetool-aarch64` and `runtime-aarch64` pins, and
  `browsecraft-<ver>-linux-aarch64.AppImage` naming.
- Flatpak (`.flatpak`, app id `io.github.matteomicheletti.browsecraft`) would
  solve the dependency and glibc issues (GNOME runtime ships WebKitGTK 4.1) but
  nests WebKit's bubblewrap sandbox inside Flatpak's, which needs
  `--talk-name=org.freedesktop.Flatpak`/portal support; to be evaluated.
- The AppImage is not signed; integrity relies on `SHA256SUMS.txt` over HTTPS.
