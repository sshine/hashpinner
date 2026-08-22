#!/usr/bin/env bash
set -euo pipefail

raw=${1:?usage: trim.sh RAW.gif OUT.gif}
out=${2:?usage: trim.sh RAW.gif OUT.gif}

hold_output=300
hold_wait=60
change_output=5000

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

gifsicle --unoptimize "$raw" -O3 -o "$work/merged.gif"
gifsicle --unoptimize "$work/merged.gif" -o "$work/full.gif"

gifsicle --info "$work/merged.gif" | awk '
    /^  \+ image #/ {
        split($4, size, "x")
        area[++frames] = size[1] * size[2]
    }
    /delay [0-9.]/ {
        for (field = 1; field <= NF; field++) {
            if ($field == "delay") {
                seconds = $(field + 1)
                sub("s$", "", seconds)
                delay[frames] = seconds
            }
        }
    }
    END {
        for (frame = 1; frame <= frames; frame++) {
            printf "%d %d\n", delay[frame] * 100 + 0.5, area[frame + 1]
        }
    }
' > "$work/frames.txt"

clamped=()
index=0
while read -r centiseconds change; do
    if [ "${change:-0}" -gt "$change_output" ]; then
        cap=$hold_wait
    else
        cap=$hold_output
    fi
    if [ "$centiseconds" -gt "$cap" ]; then
        centiseconds=$cap
    fi
    clamped+=("-d$centiseconds" "#$index")
    index=$((index + 1))
done < "$work/frames.txt"

gifsicle "$work/full.gif" "${clamped[@]}" --loopcount=forever -O3 -o "$out"
