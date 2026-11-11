# Third-party notices

## uBlock Origin

BrowseCraft's `src/adblock.rs` identifies adaptations of uBlock Origin's
`src/js/static-net-filtering.js`, `static-filtering-parser.js`,
`cosmetic-filtering.js`, and content-script behavior.

uBlock Origin — a comprehensive, efficient content blocker

Copyright (C) 2014-present Raymond Hill and the respective contributors.

Upstream: <https://github.com/gorhill/uBlock>

These upstream files are licensed under the GNU General Public License,
version 3 or (at your option) any later version. BrowseCraft is distributed
under the same GPL-3.0-or-later terms. The complete GPLv3 text is in [LICENSE](LICENSE).
The software is provided without warranty, including the implied warranties
of merchantability or fitness for a particular purpose.

This notice identifies the documented adaptation; upstream copyright and
license notices must be retained when redistributing corresponding source.

## ArtCraft Launcher (custom title bar)

`src/titlebar.rs` adapts the custom title bar widgets of ArtCraft Launcher
(`crates/ui-egui/src/widgets.rs`: the drag band under the header's controls,
double-click to maximize, the stroked caption buttons and the resize bands
with their corner rules), rewritten for a raw winit window instead of eframe
viewport commands.

Copyright (c) 2026 ArtCraft Team and the ArtCraft Launcher contributors.

Upstream is dual-licensed under the MIT license or the Apache License 2.0, at
your option; the adapted code is used under the MIT license:

> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.

## Rust dependencies and embedded assets

Rust dependencies retain their own licenses and notices. The allowlist in
`deny.toml` is a dependency review policy, not a replacement for those terms.
In particular, bundled fonts carry their own font-license requirements.
Use `cargo metadata --locked --format-version 1` to inventory package licenses
and preserve the applicable notices/license texts when packaging releases.

## Dark Reader (smart dark mode engine)

`src/darkmode/vendor/darkreader.js` is the dynamic theme engine of Dark
Reader, npm package `darkreader` 4.9.135 (<https://github.com/darkreader/darkreader>,
<https://darkreader.org>), vendored with one small patch (it no longer defines a
global `chrome` object); see `src/darkmode/vendor/README.md` for the tarball
hashes. BrowseCraft's "Dark mode" feature is built on it. The name and logo of
Dark Reader are not used for BrowseCraft's own feature.

MIT License

Copyright (c) 2026 Dark Reader Ltd.

All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
