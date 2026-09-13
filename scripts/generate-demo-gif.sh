#!/usr/bin/env bash
set -euo pipefail

output=${1:-docs/assets/forge-pipeline.gif}
frame_dir=$(mktemp -d "${TMPDIR:-/tmp}/forge-gif.XXXXXX")
trap 'rm -rf "$frame_dir"' EXIT

mkdir -p "$(dirname "$output")"

labels=("SOURCE" "SSA IR" "VERIFY" "OPTIMIZE" "ALLOCATE" "EMIT" "W^X JIT")
xs=(56 228 400 572 744 916 1088)
ys=(245 245 245 245 245 245 245)
width=1280
height=720
font_path=/System/Library/Fonts/SFNSMono.ttf
if [[ ! -f "$font_path" ]]; then
  font_path=/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf
fi

for active in "${!labels[@]}"; do
  args=(
    -size "${width}x${height}"
    "xc:#0b1020"
    -font "$font_path"
    -gravity NorthWest
    -stroke none
    -fill "#f8fafc"
    -pointsize 46
    -annotate +56+52 "FORGE / COMPILER PIPELINE"
    -fill "#94a3b8"
    -pointsize 22
    -annotate +58+118 "typed source → verified IR → portable and native artifacts"
    -stroke "#334155"
    -strokewidth 4
  )

  for ((index = 0; index < ${#labels[@]} - 1; index++)); do
    x1=$((xs[index] + 138))
    x2=$((xs[index + 1] - 14))
    args+=(-draw "line ${x1},${ys[index]} ${x2},${ys[index]}")
  done

  for ((index = 0; index < ${#labels[@]}; index++)); do
    color="#16213a"
    border="#334155"
    if ((index == active)); then
      color="#0e7490"
      border="#67e8f9"
    fi
    x_end=$((xs[index] + 138))
    y_end=$((ys[index] + 96))
    text_x=$((xs[index] + 12))
    text_y=$((ys[index] + 55))
    args+=(
      -fill "$color"
      -stroke "$border"
      -strokewidth 3
      -draw "roundrectangle ${xs[index]},${ys[index]},${x_end},${y_end},16,16"
      -stroke none
      -fill "#f8fafc"
      -pointsize 19
      -annotate "+${text_x}+${text_y}" "${labels[index]}"
    )
  done

  args+=(
    -fill "#64748b"
    -pointsize 18
    -annotate +58+430 "interpreter oracle  •  handwritten encoders  •  W^X memory  •  Workbench artifacts"
    -fill "#67e8f9"
    -pointsize 20
    -annotate +58+624 "auditable by design"
    "${frame_dir}/frame-${active}.png"
  )
  magick "${args[@]}"
done

magick -delay 85 -loop 0 "${frame_dir}"/frame-*.png -layers Optimize "$output"
