#!/usr/bin/env bash
# Builds BrowseCraft and installs it for the current user (no root):
#   ~/.local/opt/browsecraft/{browsecraft,filters/}
#   ~/.local/bin/browsecraft -> the binary
#   ~/.local/share/applications/io.github.matteomicheletti.browsecraft.desktop
#   ~/.local/share/icons/hicolor/*/apps/io.github.matteomicheletti.browsecraft.*
# Usage: scripts/install-local.sh [--uninstall]
set -euo pipefail
ID=io.github.matteomicheletti.browsecraft
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OPT=$HOME/.local/opt/browsecraft
APPS=$HOME/.local/share/applications
ICONS=$HOME/.local/share/icons/hicolor

if [[ ${1:-} == --uninstall ]]; then
  rm -rf "$OPT" "$HOME/.local/bin/browsecraft" "$APPS/$ID.desktop" "$APPS/browsecraft.desktop"
  rm -f "$ICONS"/*/apps/$ID.* "$ICONS/256x256/apps/browsecraft.png"
  update-desktop-database "$APPS" 2>/dev/null || true
  echo "BrowseCraft removed (profile data in ~/.local/share/BrowseCraft and settings in ~/.config/BrowseCraft are kept)."
  exit 0
fi

cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$OPT" "$HOME/.local/bin" "$APPS"
install -m 0755 "$ROOT/target/release/browsecraft" "$OPT/browsecraft"
rm -rf "$OPT/filters" && cp -r "$ROOT/filters" "$OPT/filters"
ln -sf "$OPT/browsecraft" "$HOME/.local/bin/browsecraft"
for size in 128x128 256x256 scalable; do
  mkdir -p "$ICONS/$size/apps"
  cp "$ROOT/assets/app-icon/hicolor/$size/apps/"$ID.* "$ICONS/$size/apps/"
done
sed "s|^Exec=browsecraft|Exec=$OPT/browsecraft|" "$ROOT/packaging/linux/$ID.desktop" > "$APPS/$ID.desktop"
# The pre-fork entry used another file name.
rm -f "$APPS/browsecraft.desktop" "$ICONS/256x256/apps/browsecraft.png"
update-desktop-database "$APPS" 2>/dev/null || true
gtk-update-icon-cache -f -t "$ICONS" 2>/dev/null || true
echo "Installed: $("$OPT/browsecraft" --version) -> $OPT"
