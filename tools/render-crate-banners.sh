#!/usr/bin/env bash
# One banner per published crate, from tools/crate-banners.tsv, in the style
# of assets/readme-banner.png (tools/render-banner.sh renders that one).
#
# Same motif, different constellation: the lit cells in the grid are derived
# from the crate name, so the banners are recognisably one family without
# being 27 copies of the same image.
set -euo pipefail
cd "$(dirname "$0")/.."
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
OUT=assets/banners
mkdir -p "$OUT"
urlenc() { python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1]))' "$1"; }

# Five of sixteen cells, chosen by hashing the crate name. Deterministic, so a
# re-render never reshuffles a banner that is already committed.
cells() {
  python3 - "$1" <<'PYEOF'
import hashlib, sys
h = hashlib.sha256(sys.argv[1].encode()).digest()
picked, i = [], 0
while len(picked) < 5:
    c = h[i] % 16
    if c not in picked:
        picked.append(c)
    i += 1
print(",".join(str(c) for c in sorted(picked)))
PYEOF
}

n=0
while IFS=$'\t' read -r crate kicker title sub meta out; do
  [ -z "${crate:-}" ] && continue
  l=$(cells "$crate")
  "$CHROME" --headless --disable-gpu --no-sandbox --hide-scrollbars \
    --force-device-scale-factor=2 --window-size=1280,400 \
    --virtual-time-budget=10000 --screenshot="$OUT/$crate.png" \
    "file://$PWD/tools/banner-render.html?h=400&k=$(urlenc "$kicker")&t=$(urlenc "$title")&s=$(urlenc "$sub")&m=$(urlenc "$meta")&o=$(urlenc "$out")&l=$l" \
    >/dev/null 2>&1
  printf '%-34s %s  cells %s\n' "$crate" "$(sips -g pixelWidth -g pixelHeight "$OUT/$crate.png" | tail -2 | tr -d ' \n')" "$l"
  n=$((n+1))
done < tools/crate-banners.tsv
echo "rendered $n banners into $OUT"
