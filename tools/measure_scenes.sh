#!/usr/bin/env bash
# Render every scene wallpaper headlessly and tally what came out.
#
# This is the corpus measurement for the scene renderer: one line per wallpaper,
# with the numbers `scenerender` reports, so coverage is a fact rather than a
# claim. Runs only on the surfaceless GL context - nothing appears on a monitor.
#
# Usage: tools/measure_scenes.sh [workshop-dir] [out.tsv]
set -u

W="${1:-$HOME/.steam/root/steamapps/workshop/content/431960}"
OUT="${2:-/tmp/scene_measure.tsv}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO/target/release/examples/scenerender"

if [ ! -x "$BIN" ]; then
    echo "building scenerender..." >&2
    (cd "$REPO" && cargo build -q --release --example scenerender -p hyprwpe-render) || exit 1
fi

printf 'id\tstatus\tjson\n' > "$OUT"

for dir in "$W"/*/; do
    id="$(basename "$dir")"
    pkg="$dir/scene.pkg"
    [ -f "$pkg" ] || continue
    # 480x270: enough to tell covered from bare, small enough to run them all.
    # `--json` is one object per line, so a corpus run is a data file. `--time 6`
    # matters: at t=0 particles have not spawned and animation is at its first
    # keyframe, so a scene is measured as blank when it is merely waiting.
    json="$("$BIN" "$pkg" /dev/null 480 270 fill --time 6 --json 2>/dev/null | tail -1)"
    if [ -z "$json" ]; then
        printf '%s\tfailed\t\n' "$id" >> "$OUT"
        continue
    fi
    printf '%s\trendered\t%s\n' "$id" "$json" >> "$OUT"
done

echo "wrote $OUT" >&2
count="$(($(wc -l < "$OUT") - 1))"
echo "$count wallpapers recorded"
