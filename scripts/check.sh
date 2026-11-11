#!/usr/bin/env bash
# Local CI (the owner policy is no cloud CI): build, test, clippy, deny, fmt.
# Usage: scripts/check.sh [--quick]   (--quick skips tests; CLIPPY_STRICT=1 denies all clippy warnings)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
step() { printf '\n==> %s\n' "$*"; }

step "cargo build --locked"
cargo build --locked
if [[ ${1:-} != --quick ]]; then
  step "cargo test --locked"
  cargo test --locked
fi
# The repo's own deny lints (unwrap/expect) always fail; plain warnings are
# existing debt, enforced only with CLIPPY_STRICT=1.
step "cargo clippy --locked --all-targets"
cargo clippy --locked --all-targets ${CLIPPY_STRICT:+-- -D warnings}

if command -v cargo-deny >/dev/null; then
  step "cargo deny check"
  cargo deny check
else
  echo "(cargo-deny not installed: skipped)"
fi

# Formatting is enforced only for files changed against main, so the legacy
# code base is not reformatted wholesale.
base=$(git merge-base HEAD main 2>/dev/null || true)
if [[ -n $base ]]; then
  mapfile -t changed < <({ git diff --name-only "$base" -- '*.rs'; git ls-files --others --exclude-standard -- '*.rs'; } | sort -u)
  existing=()
  for f in "${changed[@]}"; do [[ -f $f ]] && existing+=("$f"); done
  if [[ ${#existing[@]} -gt 0 ]]; then
    step "rustfmt --check on ${#existing[@]} changed file(s)"
    rustfmt --edition 2024 --check "${existing[@]}"
  fi
fi
step "all checks passed"
