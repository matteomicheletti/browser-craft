#!/usr/bin/env bash
# Cuts a release locally. Nothing leaves this machine unless --publish is given.
# Usage: scripts/release.sh <version> [--dry-run] [--publish] [--bump] [--skip-checks]
#   <version>      e.g. 0.2.0 (no leading v); must equal Cargo.toml's version
#                  (--bump edits Cargo.toml and Cargo.lock after confirmation)
#   --dry-run      only print what would happen, change nothing
#   --publish      push the tag and run `gh release create` (explicit action)
#   --skip-checks  do not run scripts/check.sh
set -euo pipefail
REPO=matteomicheletti/browser-craft
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

VERSION= DRY=0 PUBLISH=0 BUMP=0 CHECKS=1
for a in "$@"; do
  case $a in
    --dry-run) DRY=1 ;;
    --publish) PUBLISH=1 ;;
    --bump) BUMP=1 ;;
    --skip-checks) CHECKS=0 ;;
    -h|--help) sed -n 2,9p "$0"; exit 0 ;;
    -*) echo "unknown option $a" >&2; exit 2 ;;
    *) VERSION=${a#v} ;;
  esac
done
[[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$ ]] || { echo "usage: $0 <semver> [options]" >&2; exit 2; }
TAG=v$VERSION
DIST=$ROOT/dist/$VERSION
APPIMAGE=browsecraft-$VERSION-linux-x86_64.AppImage
run() { if [[ $DRY == 1 ]]; then echo "[dry-run] $*"; else "$@"; fi; }

branch=$(git rev-parse --abbrev-ref HEAD)
[[ $branch == main ]] || { echo "must be on main (on '$branch')" >&2; [[ $DRY == 1 ]] || exit 1; }
[[ -z $(git status --porcelain) ]] || { echo "working tree is not clean" >&2; [[ $DRY == 1 ]] || exit 1; }
ONLY_PUBLISH=0
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  # A tag from an earlier run: only publishing its already built assets is allowed.
  [[ $PUBLISH == 1 && -f $DIST/$APPIMAGE && -f $DIST/SHA256SUMS.txt ]] \
    || { echo "tag $TAG already exists (rerun with --publish to publish the built assets in $DIST)" >&2; exit 1; }
  [[ $(git rev-parse "$TAG^{commit}") == "$(git rev-parse HEAD)" ]] || { echo "tag $TAG does not point at HEAD" >&2; exit 1; }
  ONLY_PUBLISH=1
fi

if [[ $ONLY_PUBLISH == 0 ]]; then
current=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
if [[ $current != "$VERSION" ]]; then
  [[ $BUMP == 1 ]] || { echo "Cargo.toml is at $current, not $VERSION (use --bump to change it)" >&2; exit 1; }
  if [[ $DRY == 1 ]]; then
    echo "[dry-run] would bump Cargo.toml $current -> $VERSION and commit"
  else
    read -r -p "Bump Cargo.toml from $current to $VERSION and commit? [y/N] " ok
    [[ $ok == y || $ok == Y ]] || exit 1
    sed -i "0,/^version = \".*\"/s//version = \"$VERSION\"/" Cargo.toml
    cargo update --workspace --offline
    git commit -m "chore(release): $VERSION" Cargo.toml Cargo.lock
  fi
fi

if [[ $CHECKS == 1 ]]; then run scripts/check.sh; fi
run scripts/package-appimage.sh --out "$DIST"

if [[ $DRY == 1 ]]; then
  echo "[dry-run] (cd $DIST && sha256sum $APPIMAGE* > SHA256SUMS.txt)"
else
  (cd "$DIST" && rm -f SHA256SUMS.txt && sha256sum browsecraft-"$VERSION"-* > SHA256SUMS.txt && cat SHA256SUMS.txt)
fi
run git tag -a "$TAG" -m "BrowseCraft $VERSION"
fi

assets=("$DIST/$APPIMAGE")
[[ -f $DIST/$APPIMAGE.zsync || $DRY == 1 ]] && assets+=("$DIST/$APPIMAGE.zsync")
assets+=("$DIST/SHA256SUMS.txt")
CMD=(gh release create "$TAG" "${assets[@]}" --repo "$REPO" --title "BrowseCraft $VERSION" --generate-notes)
[[ $VERSION == *-* ]] && CMD+=(--prerelease)

if [[ $PUBLISH == 1 && $DRY == 0 ]]; then
  git push "git@github.com:$REPO.git" "$TAG"
  "${CMD[@]}"
else
  echo
  if [[ $DRY == 1 ]]; then echo "Would create local tag $TAG (nothing pushed). To publish:"; else echo "Local tag $TAG created (not pushed). To publish:"; fi
  printf '  git push origin %s\n  ' "$TAG"; printf '%q ' "${CMD[@]}"; echo
  echo "or run: scripts/release.sh $VERSION --publish (publishes the assets built above)"
fi
