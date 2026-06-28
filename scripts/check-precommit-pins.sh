#!/usr/bin/env bash
# Fail if third-party pre-commit hook repositories use mutable revs.
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

config=".pre-commit-config.yaml"
fail=0
repo=""
repo_line=0
repo_has_rev=0
line_num=0

trim_yaml_value() {
  local value
  value="$1"
  value="${value%%#*}"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  value="${value%\"}"
  value="${value#\"}"
  value="${value%\'}"
  value="${value#\'}"
  printf '%s' "$value"
}

is_remote_repo() {
  case "$1" in
    http://*|https://*|git://*|ssh://*|git@*) return 0 ;;
    *) return 1 ;;
  esac
}

finish_repo() {
  if [ -n "$repo" ] && is_remote_repo "$repo" && [ "$repo_has_rev" = 0 ]; then
    printf 'MISSING: %s:%s -> %s has no immutable rev\n' "$config" "$repo_line" "$repo" >&2
    fail=1
  fi
}

while IFS= read -r line || [ -n "$line" ]; do
  line_num=$((line_num + 1))
  case "$line" in
    *"- repo:"*)
      finish_repo
      repo="$(trim_yaml_value "${line#*:}")"
      repo_line=$line_num
      repo_has_rev=0
      ;;
    *"rev:"*)
      if [ -n "$repo" ] && is_remote_repo "$repo"; then
        rev="$(trim_yaml_value "${line#*:}")"
        repo_has_rev=1
        if ! printf '%s' "$rev" | grep -qE '^[0-9a-f]{40}$'; then
          printf 'UNPINNED: %s -> %s rev %s (pin to a full commit SHA, keep the tag as a trailing comment)\n' \
            "$config" "$repo" "$rev" >&2
          fail=1
        fi
      fi
      ;;
  esac
done < "$config"
finish_repo

if [ "$fail" = 0 ]; then
  echo "check-precommit-pins: OK - all remote pre-commit hooks are pinned to commit SHAs"
else
  echo "check-precommit-pins: FAILED - unpinned remote pre-commit hook ref(s) above." >&2
  echo "Resolve a tag SHA with: git ls-remote <repo-url> refs/tags/<tag> 'refs/tags/<tag>^{}'" >&2
  exit 1
fi
