#!/usr/bin/env bash
# Remove Hellbox resources and local state.
#
#   ./uninstall.sh
set -uo pipefail

DEFAULT_REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
HELLBOX_HOME_DIR="${HELLBOX_HOME:-$HOME/.hellbox}"
LEGACY_HOME_DIR="${LAMBDADOOM_HOME:-$HOME/.lambdadoom}"

say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
FAILED=0
SCRIPT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

home_specs=("hellbox|$HELLBOX_HOME_DIR|${HELLBOX_NAME:-doom}")
if { [ -z "${HELLBOX_HOME:-}" ] || [ -n "${LAMBDADOOM_HOME:-}" ]; } && [ "$LEGACY_HOME_DIR" != "$HELLBOX_HOME_DIR" ]; then
  home_specs+=("lambdadoom|$LEGACY_HOME_DIR|${LAMBDADOOM_NAME:-doom}")
fi

if [ -n "${HELLBOX_STACK:-}" ] || [ -n "${LAMBDADOOM_STACK:-}" ]; then
  stack_specs=()
  [ -n "${HELLBOX_STACK:-}" ] && stack_specs+=("hellbox|$HELLBOX_HOME_DIR|$HELLBOX_STACK")
  [ -n "${LAMBDADOOM_STACK:-}" ] && stack_specs+=("lambdadoom|$LEGACY_HOME_DIR|$LAMBDADOOM_STACK")
else
  stack_specs=("hellbox|$HELLBOX_HOME_DIR|Hellbox")
  if { [ -z "${HELLBOX_HOME:-}" ] || [ -n "${LAMBDADOOM_HOME:-}" ]; } && [ "$LEGACY_HOME_DIR" != "$HELLBOX_HOME_DIR" ]; then
    stack_specs+=("lambdadoom|$LEGACY_HOME_DIR|LambdaDoom")
  fi
fi

region_for_home(){
  local d="$1" default_region="$2" r
  if [ -f "$d/config.toml" ]; then
    r="$(grep -E '^region' "$d/config.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
    if [ -n "$r" ]; then
      printf '%s' "$r"
      return
    fi
  fi
  printf '%s' "$default_region"
}

stack_missing(){
  case "$1" in
    *"does not exist"*|*"doesn't exist"*) return 0 ;;
    *) return 1 ;;
  esac
}

cli_for_home(){
  local kind="$1" dir="$2" c
  local -a candidates
  if [ "$kind" = "lambdadoom" ]; then
    candidates=(
      "${LDOOM_BIN:-}"
      "${LAMBDADOOM_BIN:-}"
      "${HELLBOX_BIN:-}"
      "$dir/bin/ldoom"
      "$dir/bin/ldoom.exe"
      "$dir/bin/hellbox"
      "$dir/bin/hellbox.exe"
      "rs-cli/target/release/ldoom"
      "rs-cli/target/release/ldoom.exe"
      "rs-cli/target/release/hellbox"
      "rs-cli/target/release/hellbox.exe"
    )
  else
    candidates=(
      "${HELLBOX_BIN:-}"
      "${LAMBDADOOM_BIN:-}"
      "${LDOOM_BIN:-}"
      "$dir/bin/hellbox"
      "$dir/bin/hellbox.exe"
      "$dir/bin/ldoom"
      "$dir/bin/ldoom.exe"
      "rs-cli/target/release/hellbox"
      "rs-cli/target/release/hellbox.exe"
      "rs-cli/target/release/ldoom"
      "rs-cli/target/release/ldoom.exe"
    )
  fi
  for c in "${candidates[@]}"; do
    [ -n "$c" ] && [ -x "$c" ] && { printf '%s' "$c"; return; }
  done
}

canonical_dir(){
  local d="$1"
  [ -d "$d" ] || return 1
  (cd "$d" 2>/dev/null && pwd -P)
}

path_is_same_or_parent(){
  local parent="$1" child="$2"
  [ "$parent" = "$child" ] && return 0
  case "$child" in
    "$parent"/*) return 0 ;;
    *) return 1 ;;
  esac
}

home_has_marker(){
  local d="$1"
  if [ -f "$d/config.toml" ] \
    && grep -qE '^[[:space:]]*artifact_bucket[[:space:]]*=' "$d/config.toml" \
    && grep -qE '^[[:space:]]*execution_role_arn[[:space:]]*=' "$d/config.toml"; then
    return 0
  fi
  return 1
}

remove_local_home(){
  local d="$1" display="$1" resolved home_resolved
  if [ -z "$d" ]; then
    warn "refusing to remove an empty local-state path"
    return 1
  fi
  if [ ! -e "$d" ]; then
    say "Local state not found, skipping: $display"
    return 0
  fi
  if [ -L "$d" ]; then
    warn "refusing to remove symlinked local-state path: $display"
    return 1
  fi
  if [ ! -d "$d" ]; then
    warn "refusing to remove non-directory local-state path: $display"
    return 1
  fi
  if ! resolved="$(canonical_dir "$d")"; then
    warn "refusing to remove unresolvable local-state path: $display"
    return 1
  fi
  if [ -z "$resolved" ] || [ "$resolved" = "/" ]; then
    warn "refusing to remove dangerous local-state path: $display"
    return 1
  fi
  home_resolved="$(canonical_dir "$HOME" 2>/dev/null || true)"
  if [ -n "$home_resolved" ] && path_is_same_or_parent "$resolved" "$home_resolved"; then
    if [ "$resolved" = "$home_resolved" ]; then
      warn "refusing to remove the user's home directory: $display"
    else
      warn "refusing to remove a parent of the user's home directory: $display"
    fi
    return 1
  fi
  if path_is_same_or_parent "$resolved" "$SCRIPT_ROOT"; then
    if [ "$resolved" = "$SCRIPT_ROOT" ]; then
      warn "refusing to remove the repository root: $display"
    else
      warn "refusing to remove a parent of the repository root: $display"
    fi
    return 1
  fi
  if ! home_has_marker "$resolved"; then
    warn "refusing to remove $display: no Hellbox config marker found"
    return 1
  fi

  say "Removing $resolved  (binary, config, state)"
  rm -rf -- "$resolved"
}

# Stop a running loopback proxy (`hellbox open`) if one is still up — uninstall
# should be self-contained whether or not the user Ctrl-C'd it first. Targets only
# the hellbox proxy by command pattern; harmless if none is running.
if command -v pkill >/dev/null 2>&1; then
  if pkill -f 'hellbox(\.exe)? open' 2>/dev/null; then
    say "Stopped the running hellbox proxy"
  fi
fi

# AWS teardown is opt-in and confirmed. Removing the CLI must never silently
# delete a user's cloud resources. Default is to KEEP everything in AWS; the
# user (or HELLBOX_YES=1 for scripts) has to say yes to tear it down.
REMOVE_AWS=0
if [ "${HELLBOX_YES:-0}" = "1" ]; then
  REMOVE_AWS=1
  say "HELLBOX_YES=1 set: will remove AWS resources (MicroVM, image, bucket, stack)."
elif [ -t 0 ]; then
  printf '\n\033[1;33mThis can also delete your Hellbox AWS resources:\033[0m\n'
  printf '  - the DOOM MicroVM and its image\n'
  printf '  - the CloudFormation stack (%s) and its S3 artifact bucket\n' "${stack_specs[0]##*|}"
  printf 'These live in YOUR AWS account. Deleting them is irreversible.\n'
  printf 'Remove them now? [y/N] '
  read -r reply
  case "$reply" in
    [yY]|[yY][eE][sS]) REMOVE_AWS=1 ;;
    *) say "Keeping all AWS resources. Run \`hellbox destroy\` (or re-run with HELLBOX_YES=1) to remove them later." ;;
  esac
else
  # No terminal to ask and no explicit yes: keep AWS resources, never guess.
  say "Non-interactive shell: keeping AWS resources. Set HELLBOX_YES=1 to remove them, or run \`hellbox destroy\`."
fi

if [ "$REMOVE_AWS" = 1 ]; then

# MicroVM + image.
for spec in "${home_specs[@]}"; do
  IFS='|' read -r kind d name <<<"$spec"
  [ -d "$d" ] || continue
  DOOM="$(cli_for_home "$kind" "$d")"
  if [ -n "$DOOM" ]; then
    say "Removing the DOOM microvm + image using state in $d"
    HELLBOX_HOME="$d" LAMBDADOOM_HOME="$d" "$DOOM" rm --name "$name" \
      || say "(nothing to remove, or already gone)"
  else
    say "hellbox/ldoom CLI not found for $d — skipping microvm/image cleanup (delete the image manually if one exists)"
  fi
done

# Stack.
for spec in "${stack_specs[@]}"; do
  IFS='|' read -r kind d stack <<<"$spec"
  REGION="$(region_for_home "$d" "$DEFAULT_REGION")"
  describe_output=""
  if ! describe_output="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$stack" 2>&1)"; then
    if stack_missing "$describe_output"; then
      say "CloudFormation stack not found in $REGION, skipping: $stack"
    else
      warn "could not describe CloudFormation stack '$stack' in $REGION — leaving local state in place; AWS said: $describe_output"
      FAILED=1
    fi
    continue
  fi
  if ! BUCKET="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$stack" \
    --query "Stacks[0].Outputs[?OutputKey=='ArtifactBucket'].OutputValue" --output text 2>&1)"; then
    warn "could not read artifact bucket for '$stack' in $REGION — leaving local state in place; AWS said: $BUCKET"
    FAILED=1
    continue
  fi
  if [ -n "$BUCKET" ] && [ "$BUCKET" != "None" ]; then
    say "Emptying artifact bucket: $BUCKET"
    # Surface the real error: a non-empty bucket blocks stack deletion, and a
    # silently-failed empty leaves billing resources behind.
    if ! aws s3 rm "s3://$BUCKET" --recursive >/dev/null; then
      warn "could not empty s3://$BUCKET — delete its objects manually, else stack deletion will fail"
      FAILED=1
    fi
  fi
  say "Deleting CloudFormation stack: $stack"
  if ! aws cloudformation delete-stack --region "$REGION" --stack-name "$stack"; then
    warn "delete-stack call failed for '$stack' in $REGION — check the CloudFormation console"
    FAILED=1
  elif ! aws cloudformation wait stack-delete-complete --region "$REGION" --stack-name "$stack"; then
    warn "stack '$stack' did not finish deleting — check the CloudFormation console (resources may remain and still bill)"
    FAILED=1
  fi
done

fi  # REMOVE_AWS

if [ "$FAILED" = 1 ]; then
  warn "uninstall finished with errors — local state was left in place so cleanup can be retried after fixing AWS access."
  warn "verify in the AWS console that the stack, bucket, and any MicroVM/image are gone (they may still incur cost)."
  exit 1
fi

# Local state.
for spec in "${home_specs[@]}"; do
  IFS='|' read -r _ d _ <<<"$spec"
  if ! remove_local_home "$d"; then
    FAILED=1
  fi
done

if [ "$FAILED" = 1 ]; then
  warn "uninstall refused to remove one or more local-state paths; inspect the warnings above."
  exit 1
fi

say "Hellbox removed. Delete your clone of the repo if you no longer need it."
