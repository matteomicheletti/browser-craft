#!/bin/sh
# Regenerates the scripts and screenshots every test page with each variant.
# usage: tools/darkmode-harness/run.sh OUT_DIR
set -e
cd "$(dirname "$0")/../.."
OUT=${1:?out dir}
mkdir -p "$OUT/scripts" "$OUT/shots"
BC_DARK_DUMP_DIR="$OUT/scripts" cargo test dump_scripts -- --ignored >/dev/null
for page in light native-dark variables shadow; do
  python3 tools/darkmode-harness/shoot.py none "$PWD/tests/web/darkmode/$page.html" "$OUT/shots/$page-off.png"
  for v in dark dark-keep-native-off dimmed system-light system-dark; do
    python3 tools/darkmode-harness/shoot.py "$OUT/scripts/$v.js" "$PWD/tests/web/darkmode/$page.html" "$OUT/shots/$page-$v.png"
  done
done
