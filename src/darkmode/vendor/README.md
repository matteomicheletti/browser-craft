# Vendored Dark Reader

- Package: `darkreader` 4.9.135 (npm), MIT, https://github.com/darkreader/darkreader
- Tarball: https://registry.npmjs.org/darkreader/-/darkreader-4.9.135.tgz
  - sha1   2dbcb11b6eb0eae383af28a9b214eb1caa24064c (matches the registry)
  - sha256 e1448dbb485a7d7f7ce879cb71240c70f195e2c25349b0e8abc1a4108f4ec8e5
- `darkreader.js` is the package's UMD build with ONE patch: the
  `if (!window.chrome) { window.chrome = {}; }` block is replaced by
  `const chrome = {};` so no global `chrome` object is added to pages.
  Re-vendoring: download the tarball, check the hashes, apply that patch.
- `LICENSE.darkreader` is the LICENSE file from the tarball, unchanged.
