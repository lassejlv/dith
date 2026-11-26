#!/bin/sh
# Rebuild the cat icon and its blink with dith alone: hand-written SVGs in, PNG and GIF out.
# Usage: sh build.sh            (writes to ./out)
#        DITH=path/to/dith OUT=/tmp/cat sh build.sh
set -eu
cd "$(dirname "$0")"
DITH="${DITH:-dith}"
OUT="${OUT:-out}"
mkdir -p "$OUT"

# Shared look: two-colour Atkinson on 2px pixels.
STYLE="--algorithm atkinson --colors #F5F1EA,#171717 --pixel-size 2 --bias 0.12 --no-auto-levels"
# The body: head and ears inflated and melted together, lit from the top left, blush painted in.
BODY="--shape body.svg --blend 24 --paint blush.svg --angle 135 --ground-shadow --ambient 0.5 --depth 0.6 --specular 0.3 --stops #171717,#F5F1EA --canvas-width 512 --canvas-height 512"

# 1. The icon: dithered body plus exact ink line work.
"$DITH" generate inflate $BODY $STYLE --overlay face.svg --overlay eyes-open.svg -o "$OUT/cat.png"

# 2. The blink: four frames held 4200/50/100/50 ms (20 fps steps, 4.4 s loop). The light stays
#    put (--cycles 0), only the eye overlay changes, and locked-diff keeps every other pixel.
"$DITH" animate inflate $BODY $STYLE --cycles 0 --frames 4 --fps 20 --timings blink.json \
  --temporal locked-diff --overlay face.svg \
  --overlay-frames eyes-open.svg,eyes-half.svg,eyes-closed.svg,eyes-half.svg \
  -o "$OUT/cat-blink.gif"

# 3. Proof: every pixel that changes between consecutive frames (and across the loop seam).
"$DITH" --json diff --frames "$OUT/cat-blink.gif" --loop -o "$OUT/blink-changes.png" > "$OUT/blink-diff.json"
