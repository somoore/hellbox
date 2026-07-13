#!/usr/bin/env bash
# Verify release assets remain bound to tag-source attestations.
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

fail=0

require_pattern(){
  local file pattern message
  file="$1"; pattern="$2"; message="$3"
  if ! grep -qE -- "$pattern" "$file"; then
    printf 'MISSING: %s (%s)\n' "$message" "$file" >&2
    fail=1
  fi
}

reject_pattern(){
  local file pattern message
  file="$1"; pattern="$2"; message="$3"
  if grep -qE -- "$pattern" "$file"; then
    printf 'UNSAFE: %s (%s)\n' "$message" "$file" >&2
    fail=1
  fi
}

require_pattern ".github/workflows/release.yml" "tags: \\['v\\*'\\]" \
  "release workflow must be triggered by version tags"
reject_pattern ".github/workflows/release.yml" "workflow_dispatch:" \
  "manual release dispatch must not publish arbitrary tag inputs"
reject_pattern ".github/workflows/release.yml" "github\\.event\\.inputs\\.tag" \
  "release publishing must not use user-supplied tag inputs"
require_pattern ".github/workflows/release.yml" "tag_name: \\$\\{\\{ github\\.ref_name \\}\\}" \
  "release publishing must attach assets to the source ref tag"
require_pattern ".github/workflows/release.yml" "GITHUB_REF_TYPE.*tag" \
  "release workflow must fail closed unless the source ref is a tag"

require_pattern "deploy.sh" "source_ref=\"refs/tags/\\\$rel\"" \
  "deploy attestation verification must derive a tag source ref"
require_pattern "deploy.sh" "--source-ref \"\\\$source_ref\"" \
  "deploy attestation verification must bind to the expected source ref"

require_pattern "install.ps1" "--source-ref \"refs/tags/\\\$releaseTag\"" \
  "Windows install attestation must bind to the expected tag source ref"
require_pattern "install.ps1" "--signer-workflow \"github.com/\\\$Repo/" \
  "Windows install attestation must pin the release signer workflow"

if [ "$fail" = 0 ]; then
  echo "check-release-provenance: OK - release attestations are bound to tag source refs"
else
  echo "check-release-provenance: FAILED - release provenance binding regressed" >&2
  exit 1
fi
