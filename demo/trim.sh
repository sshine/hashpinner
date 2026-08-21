#!/usr/bin/env bash
set -euo pipefail

raw=${1:?usage: trim.sh RAW.gif OUT.gif}
out=${2:?usage: trim.sh RAW.gif OUT.gif}

hold_output=300
hold_wait=60
ink_output=15000

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

gifsicle --unoptimize "$raw" -O3 -o "$work/merged.gif"
gifsicle --unoptimize "$work/merged.gif" -o "$work/full.gif"
gifsicle --explode "$work/full.gif" --output "$work/f" 2> /dev/null

frames=()
index=0
while read -r delay; do
    centiseconds=$(awk -v d="$delay" 'BEGIN { printf "%d", d * 100 + 0.5 }')
    if [ "$(stat -c%s "$(printf '%s.%03d' "$work/f" "$index")")" -gt "$ink_output" ]; then
        cap=$hold_output
    else
        cap=$hold_wait
    fi
    if [ "$centiseconds" -gt "$cap" ]; then
        centiseconds=$cap
    fi
    frames+=("-d$centiseconds" "#$index")
    index=$((index + 1))
done < <(gifsicle --info "$work/merged.gif" | grep -o 'delay [0-9.]*' | cut -d' ' -f2)

gifsicle "$work/full.gif" "${frames[@]}" --loopcount=forever -O3 -o "$out"
