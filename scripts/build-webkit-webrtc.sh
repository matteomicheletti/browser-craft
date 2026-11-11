#!/usr/bin/env bash
# Rebuilds Arch's webkit2gtk-4.1 package with WebRTC turned on
# (-DENABLE_WEB_RTC=ON -DUSE_GSTREAMER_WEBRTC=ON), so that calls in
# Microsoft Teams, Google Meet, Zoom and Jitsi can work inside BrowseCraft.
#
#   !!! UNTESTED. Read it before running it. The owner decides. !!!
#
# Why: the Arch/Manjaro webkit2gtk-4.1 build has no RTCPeerConnection, even
# with WebKitSettings:enable-webrtc on (see docs/ARCHITECTURE_DECISION.md,
# "WebRTC on Arch/Manjaro"). Nothing in BrowseCraft can switch it on: it is
# a compile-time option of WebKit.
#
# What it does:
#   1. clones the official Arch packaging repo
#      https://gitlab.archlinux.org/archlinux/packaging/packages/webkit2gtk-4.1
#      into a temporary directory, at the tag of the installed version when
#      there is one;
#   2. adds the two CMake options to the PKGBUILD;
#   3. runs makepkg (no install). The packages are left in that directory.
#
# Needs:
#   - time: a full WebKit compile, about 1-3 hours on 8-16 cores, longer on
#     fewer; RAM: 16 GB or more (use fewer jobs, MAKEFLAGS=-j4, with less);
#   - disk: about 25-30 GB free in the build directory ($BUILD_ROOT, default
#     /var/tmp), plus the build dependencies makepkg -s installs;
#   - base-devel, git, and for WebRTC: gst-plugins-bad-libs, gst-plugins-bad,
#     libnice (ICE), libsrtp; installed with --needed below.
#
# Install (after reading the build log):
#   sudo pacman -U <dir>/webkit2gtk-4.1-<version>-x86_64.pkg.tar.zst
# Keep it from being replaced by the next update: add
#   IgnorePkg = webkit2gtk-4.1
# to /etc/pacman.conf (remove it again when you revert, or you miss
# security updates of WebKit).
#
# Revert to the distribution's build:
#   sudo pacman -S webkit2gtk-4.1
#
# Check after installing: open BrowseCraft, go to a Teams/Meet page; the
# "Calls need WebRTC" bar must not appear, and in the dev tools console
# `typeof RTCPeerConnection` is "function".
#
# Usage: scripts/build-webkit-webrtc.sh [--yes]
set -euo pipefail

REPO=https://gitlab.archlinux.org/archlinux/packaging/packages/webkit2gtk-4.1.git
BUILD_ROOT=${BUILD_ROOT:-/var/tmp}

cat <<'EOF'
This rebuilds WebKitGTK (webkit2gtk-4.1) from source with WebRTC.
  - It is UNTESTED and takes hours; it needs ~30 GB of disk and 16 GB of RAM.
  - It only builds: installing (pacman -U) is a separate step you choose.
  - A self-built WebKit no longer gets the distribution's security updates
    until you go back with: sudo pacman -S webkit2gtk-4.1
EOF
if [[ ${1:-} != --yes ]]; then
  read -r -p "Continue? [y/N] " answer
  [[ $answer == [yY] ]] || exit 1
fi

command -v makepkg >/dev/null || { echo "makepkg not found: this script is for Arch or Manjaro" >&2; exit 1; }
avail_kb=$(df -Pk "$BUILD_ROOT" | awk 'NR==2 {print $4}')
if (( avail_kb < 25 * 1024 * 1024 )); then
  echo "Less than 25 GB free in $BUILD_ROOT; set BUILD_ROOT to a bigger disk" >&2
  exit 1
fi

# Arch and Manjaro do not support partial upgrades: installing one package
# from a newer repository than the rest of the system breaks dependencies
# (e.g. gstreamer 1.28.7 against installed gst-* 1.28.6). Stop and ask for a
# full upgrade first; the build also has to match the installed libraries.
pending=$(pacman -Qu 2>/dev/null | wc -l)
if (( pending > 0 )); then
  echo "Your system has $pending pending updates. Update everything first, then run this again:" >&2
  echo "  sudo pacman -Syu        (or: pamac upgrade)" >&2
  echo "and reboot if the kernel or systemd were updated." >&2
  exit 1
fi

# Install only what is missing; never upgrade packages that are already there.
missing=$(pacman -T base-devel git gst-plugins-bad-libs gst-plugins-bad libnice libsrtp || true)
if [[ -n $missing ]]; then
  # shellcheck disable=SC2086 # one package name per word
  sudo pacman -S --needed $missing
fi

dir=$(mktemp -d "$BUILD_ROOT/webkit2gtk-webrtc.XXXXXX")
echo "Building in $dir"
git clone "$REPO" "$dir/webkit2gtk-4.1"
cd "$dir/webkit2gtk-4.1"

# Build the version that is installed (Manjaro can lag behind Arch).
installed=$(pacman -Q webkit2gtk-4.1 2>/dev/null | awk '{print $2}' || true)
if [[ -n $installed ]] && git rev-parse -q --verify "refs/tags/$installed" >/dev/null; then
  git checkout -q "$installed"
  echo "Using the packaging of the installed version $installed"
else
  echo "No tag for the installed version (${installed:-none}); using the latest packaging"
fi

# Add the options right after the GTK port line of the CMake call.
if ! grep -q -- '-DPORT=GTK' PKGBUILD; then
  echo "PKGBUILD has no -DPORT=GTK line: the packaging changed, edit it by hand" >&2
  exit 1
fi
sed -i 's/\(-DPORT=GTK\)/\1 -DENABLE_WEB_RTC=ON -DUSE_GSTREAMER_WEBRTC=ON/' PKGBUILD
# Mark the build so `pacman -Qi` shows it is not the distribution's.
sed -i 's/^pkgrel=\(.*\)/pkgrel=\1.1/' PKGBUILD
grep -n -- '-DENABLE_WEB_RTC=ON' PKGBUILD

# -s installs the build dependencies; nothing is installed from the result.
# The WebKit tarball is signature-checked: if makepkg reports an unknown
# key, import the one listed in the PKGBUILD's validpgpkeys
# (gpg --recv-keys <fingerprint>) and run again; never skip the check.
makepkg -s --noconfirm

echo
echo "Built packages:"
ls -1 "$dir/webkit2gtk-4.1"/*.pkg.tar.*
echo
echo "To install:  sudo pacman -U $dir/webkit2gtk-4.1/webkit2gtk-4.1-*.pkg.tar.zst"
echo "To revert:   sudo pacman -S webkit2gtk-4.1"
echo "When done:   rm -rf $dir"
