#!/usr/bin/env bash
# Capture demo media from a live local LambdaDoom proxy.
set -euo pipefail

URL="${LAMBDADOOM_DEMO_URL:-${1:-http://127.0.0.1:6080/?display=h264}}"
OUT_DIR="${LAMBDADOOM_DEMO_OUT:-assets/demo}"
PLAYWRIGHT_VERSION="${PLAYWRIGHT_VERSION:-1.56.1}"

say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*" >&2; }
die(){ printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v node >/dev/null || die "node is required"
command -v npm >/dev/null || die "npm is required"
command -v curl >/dev/null || die "curl is required"

curl -fsS --max-time 3 "$URL" >/dev/null \
  || die "no live LambdaDoom proxy at $URL. Start one first: ldoom open --name <name> --no-open"

mkdir -p "$OUT_DIR"
say "Capturing live demo media from $URL -> $OUT_DIR"

npm exec --yes --package "playwright@$PLAYWRIGHT_VERSION" -- \
  node scripts/capture-demo-media.mjs "$URL" "$OUT_DIR"

say "Wrote:"
find "$OUT_DIR" -maxdepth 1 -type f \( -name '*.png' -o -name '*.webm' \) -print | sort >&2
