#!/usr/bin/env bash
set -euo pipefail

# Record a small, reproducible terminal demo from the real Forge CLI.
#
# Every text panel in the resulting GIF is captured from a command executed
# against this checkout. The asset is therefore an execution recording, not
# an illustration of a compiler pipeline. Keep the source expression and
# command sequence stable so documentation rebuilds remain reviewable.

output=${1:-docs/assets/forge-pipeline.gif}
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
frame_dir=$(mktemp -d "${TMPDIR:-/tmp}/forge-demo.XXXXXX")
trap 'rm -rf "$frame_dir"' EXIT

if ! command -v magick >/dev/null 2>&1; then
  echo "error: ImageMagick (magick) is required to record the demo" >&2
  exit 1
fi

mkdir -p "$(dirname -- "$output")"

cli="$repo_root/target/debug/forge-cli"
if [[ ! -x "$cli" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" -p forge-cli --quiet
fi

expression='if x > 0.0 then sqrt(x * x + y * y) else -x'
font_path=/System/Library/Fonts/SFNSMono.ttf
if [[ ! -f "$font_path" ]]; then
  font_path=/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf
fi
if [[ ! -f "$font_path" ]]; then
  font_path=/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf
fi

capture() {
  local index=$1
  local title=$2
  local command_line=$3
  shift 3
  local text_file="$frame_dir/output-${index}.txt"

  {
    printf '$ %s\n\n' "$command_line"
    "$@"
  } >"$text_file" 2>&1

  printf '%s\n' "$title" >"$frame_dir/title-${index}.txt"
}

capture 00 '01  /  EXECUTE' \
  'forge-cli eval "if x > 0.0 then sqrt(x * x + y * y) else -x" --x 3 --y 4' \
  "$cli" eval "$expression" --x 3 --y 4
capture 01 '02  /  LOWER TO SSA' \
  'forge-cli ir "if x > 0.0 then sqrt(x * x + y * y) else -x"' \
  "$cli" ir "$expression"
capture 02 '03  /  SELECT AND ENCODE' \
  'forge-cli compile --emit "if x > 0.0 then sqrt(x * x + y * y) else -x"' \
  "$cli" compile --emit "$expression"
capture 03 '04  /  ALLOCATE REGISTERS' \
  'forge-cli regalloc "if x > 0.0 then sqrt(x * x + y * y) else -x"' \
  "$cli" regalloc "$expression"
capture 04 '05  /  VERIFY DIFFERENTIALS' \
  'forge-cli verify --iters 12 "if x > 0.0 then sqrt(x * x + y * y) else -x"' \
  "$cli" verify --iters 12 "$expression"
capture 05 '06  /  MEASURE THE RUN' \
  'forge-cli bench --sizes 1,10,100 --warmup 10 "if x > 0.0 then sqrt(x * x + y * y) else -x"' \
  "$cli" bench --sizes 1,10,100 --warmup 10 "$expression"

width=1280
height=720
for index in 00 01 02 03 04 05; do
  magick \
    -size "${width}x${height}" xc:'#0b1020' \
    -font "$font_path" -stroke none -gravity NorthWest \
    -fill '#67e8f9' -pointsize 23 -annotate +60+52 "$(<"$frame_dir/title-${index}.txt")" \
    -fill '#f8fafc' -pointsize 31 -annotate +60+105 'Forge / real execution capture' \
    -fill '#64748b' -pointsize 18 -annotate +60+150 'source → SSA → native bytes → allocation → verification → timing' \
    \( -size 1160x470 -background '#111827' -fill '#dbeafe' -font "$font_path" -pointsize 17 -gravity NorthWest caption:"@$frame_dir/output-${index}.txt" \) \
    -geometry +60+190 -composite \
    -fill '#475569' -pointsize 16 -annotate +60+684 'captured from commands run in this checkout' \
    "$frame_dir/frame-${index}.png"
done

magick -delay 115 -loop 0 "$frame_dir"/frame-*.png -layers Optimize "$output"
echo "recorded real Forge CLI demo: $output"
