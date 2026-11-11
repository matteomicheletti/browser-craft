#!/usr/bin/env bash
# Builds dist/<version>/browsecraft-<version>-linux-x86_64.AppImage (lean
# AppImage: BrowseCraft + filters/ bundled, WebKitGTK/GTK3/bubblewrap from the
# host; see docs/PACKAGING.md). Tools are downloaded once into
# ~/.cache/browsecraft-packaging and verified against pinned SHA-256 sums.
# Usage: scripts/package-appimage.sh [--no-build] [--out DIR]
set -euo pipefail

APPIMAGETOOL_VER=1.9.1
APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
RUNTIME_VER=20251108
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

ID=io.github.matteomicheletti.browsecraft
ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${BROWSECRAFT_PACKAGING_CACHE:-$HOME/.cache/browsecraft-packaging}
BUILD=1
OUT=

while [[ $# -gt 0 ]]; do
  case $1 in
    --no-build) BUILD=0 ;;
    --out) OUT=$2; shift ;;
    -h|--help) sed -n 2,6p "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

[[ $(uname -m) == x86_64 ]] || { echo "only x86_64 is supported (see docs/PACKAGING.md)" >&2; exit 1; }
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
[[ -n $VERSION ]] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }
OUT=${OUT:-$ROOT/dist/$VERSION}
NAME=browsecraft-$VERSION-linux-x86_64.AppImage

fetch() { # name url sha256
  local file=$CACHE/$1
  if [[ ! -f $file ]] || ! echo "$3  $file" | sha256sum -c --status; then
    mkdir -p "$CACHE"
    curl -fsSL --retry 3 -o "$file.part" "$2"
    echo "$3  $file.part" | sha256sum -c --status || { rm -f "$file.part"; echo "checksum mismatch for $1" >&2; exit 1; }
    mv "$file.part" "$file"
  fi
  chmod +x "$file"
}
fetch "appimagetool-$APPIMAGETOOL_VER-x86_64.AppImage" \
  "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VER/appimagetool-x86_64.AppImage" "$APPIMAGETOOL_SHA256"
fetch "runtime-$RUNTIME_VER-x86_64" \
  "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VER/runtime-x86_64" "$RUNTIME_SHA256"

if [[ $BUILD == 1 ]]; then
  cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"
fi
BIN=$ROOT/target/release/browsecraft
[[ -x $BIN ]] || { echo "missing $BIN (run without --no-build)" >&2; exit 1; }
[[ $("$BIN" --version) == "browsecraft $VERSION" ]] || { echo "binary version does not match Cargo.toml ($VERSION): rebuild" >&2; exit 1; }

# The AppDir is assembled outside the source tree.
WORK=$(mktemp -d "${TMPDIR:-/tmp}/browsecraft-appdir.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
APPDIR=$WORK/BrowseCraft.AppDir
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/metainfo"
install -m 0755 "$BIN" "$APPDIR/usr/bin/browsecraft"
cp -r "$ROOT/filters" "$APPDIR/usr/bin/filters"      # adblock reads filters/ next to the executable
install -m 0755 "$ROOT/packaging/linux/AppRun" "$APPDIR/AppRun"
sed 's|^Exec=.*|Exec=browsecraft %u|' "$ROOT/packaging/linux/$ID.desktop" > "$APPDIR/$ID.desktop"
cp "$APPDIR/$ID.desktop" "$APPDIR/usr/share/applications/"
sed -e "s/@VERSION@/$VERSION/" -e "s/@DATE@/$(date -u +%F)/" \
  "$ROOT/packaging/linux/$ID.metainfo.xml" > "$APPDIR/usr/share/metainfo/$ID.metainfo.xml"
for size in 128x128 256x256 scalable; do
  mkdir -p "$APPDIR/usr/share/icons/hicolor/$size/apps"
  cp "$ROOT/assets/app-icon/hicolor/$size/apps/"$ID.* "$APPDIR/usr/share/icons/hicolor/$size/apps/"
done
cp "$ROOT/assets/app-icon/hicolor/256x256/apps/$ID.png" "$APPDIR/$ID.png"
ln -s "$ID.png" "$APPDIR/.DirIcon"
cp "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$APPDIR/usr/share/"

mkdir -p "$OUT"
rm -f "$OUT/$NAME" "$OUT/$NAME.zsync"
ARGS=(--appimage-extract-and-run --runtime-file "$CACHE/runtime-$RUNTIME_VER-x86_64" --no-appstream)
if command -v zsyncmake >/dev/null; then
  ARGS+=(-u "gh-releases-zsync|matteomicheletti|browser-craft|latest|browsecraft-*-linux-x86_64.AppImage.zsync")
fi
(cd "$WORK" && ARCH=x86_64 "$CACHE/appimagetool-$APPIMAGETOOL_VER-x86_64.AppImage" "${ARGS[@]}" "$APPDIR" "$OUT/$NAME")
chmod 0755 "$OUT/$NAME"
# appimagetool writes the .zsync next to the cwd or the output depending on version.
[[ -f $WORK/$NAME.zsync ]] && mv "$WORK/$NAME.zsync" "$OUT/"
echo "built $OUT/$NAME ($(du -h "$OUT/$NAME" | cut -f1))"
[[ -f $OUT/$NAME.zsync ]] || echo "no zsync tool found: .zsync not produced (optional)"
