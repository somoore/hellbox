#!/usr/bin/env bash
# Remove LambdaDoom resources and local state.
#
#   ./uninstall.sh
set -uo pipefail

STACK="${LAMBDADOOM_STACK:-LambdaDoom}"
REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
NAME="${LAMBDADOOM_NAME:-doom}"
HOME_DIR="${LAMBDADOOM_HOME:-$HOME/.lambdadoom}"

say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }

# Prefer the deployed region from config.
if [ -f "$HOME_DIR/config.toml" ]; then
  r="$(grep -E '^region' "$HOME_DIR/config.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
  [ -n "$r" ] && REGION="$r"
fi

# Locate ldoom for MicroVM cleanup.
DOOM=""
for c in "${LDOOM_BIN:-}" "$HOME_DIR/bin/ldoom" "$HOME_DIR/bin/ldoom.exe" "rs-cli/target/release/ldoom" "rs-cli/target/release/ldoom.exe"; do
  [ -n "$c" ] && [ -x "$c" ] && { DOOM="$c"; break; }
done

# MicroVM + image.
if [ -n "$DOOM" ]; then
  say "Removing the DOOM microvm + image"
  "$DOOM" rm --name "$NAME" || say "(nothing to remove, or already gone)"
else
  say "ldoom CLI not found — skipping microvm/image cleanup (delete the image manually if one exists)"
fi

# Stack.
BUCKET="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='ArtifactBucket'].OutputValue" --output text 2>/dev/null || true)"
if [ -n "$BUCKET" ] && [ "$BUCKET" != "None" ]; then
  say "Emptying artifact bucket: $BUCKET"
  aws s3 rm "s3://$BUCKET" --recursive >/dev/null 2>&1 || true
fi
say "Deleting CloudFormation stack: $STACK"
aws cloudformation delete-stack --region "$REGION" --stack-name "$STACK" 2>/dev/null || true
aws cloudformation wait stack-delete-complete --region "$REGION" --stack-name "$STACK" 2>/dev/null || true

# Local state.
say "Removing $HOME_DIR  (binary, config, state)"
rm -rf "$HOME_DIR"

say "LambdaDoom removed. Delete your clone of the repo if you no longer need it."
