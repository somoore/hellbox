#!/usr/bin/env bash
# Fail if rs-cli/src/embedded.rs drifts from `git ls-files capsule/`.
# `hellbox deploy` builds the image from the embedded copy when there is no
# repo checkout, so a capsule file missing from the list ships a broken build
# context — silently. Run by CI (capsule + rust jobs).
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

EMBED=rs-cli/src/embedded.rs
fail=0

while IFS= read -r f; do
  rel="${f#capsule/}"
  if ! grep -qF "\"$rel\"" "$EMBED"; then
    echo "MISSING from $EMBED: $f (add it to CAPSULE_FILES)" >&2
    fail=1
  fi
done < <(git ls-files 'capsule/')

tracked=$(git ls-files 'capsule/' | wc -l | tr -d ' ')
embedded=$(grep -c 'include_bytes!' "$EMBED")
if [ "$tracked" -ne "$embedded" ]; then
  echo "COUNT MISMATCH: $tracked tracked capsule files vs $embedded embedded entries (stale entry in $EMBED?)" >&2
  fail=1
fi

[ "$fail" = 0 ] && echo "check-embedded-capsule: OK ($tracked files)"
exit "$fail"
