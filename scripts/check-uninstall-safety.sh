#!/usr/bin/env bash
# Regression check for uninstall.sh local-state deletion guards.
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

orig_path="${PATH:-/usr/bin:/bin}"
fakebin="$tmp/bin"
mkdir -p "$fakebin"

cat > "$fakebin/aws" <<'FAKE_AWS'
#!/usr/bin/env bash
if [ "${1:-}" = "cloudformation" ] && [ "${2:-}" = "describe-stacks" ]; then
  echo "An error occurred (ValidationError): Stack with id Hellbox does not exist" >&2
  exit 255
fi
echo "unexpected aws invocation: $*" >&2
exit 2
FAKE_AWS
chmod +x "$fakebin/aws"

cat > "$fakebin/pkill" <<'FAKE_PKILL'
#!/usr/bin/env bash
exit 1
FAKE_PKILL
chmod +x "$fakebin/pkill"

fail(){
  echo "check-uninstall-safety: FAILED - $*" >&2
  exit 1
}

make_config(){
  local dir="$1"
  mkdir -p "$dir"
  cat > "$dir/config.toml" <<'CONFIG'
region             = "us-east-1"
artifact_bucket    = "hellbox-artifacts"
build_role_arn     = "arn:aws:iam::123456789012:role/build"
execution_role_arn = "arn:aws:iam::123456789012:role/exec"
base_image_arn     = "arn:aws:lambda:us-east-1:aws:microvm-image:al2023-1"
display            = "h264"
CONFIG
}

run_uninstall(){
  local script="$1" home="$2" out="$3" err="$4"
  shift 4
  env -i \
    PATH="$fakebin:$orig_path" \
    HOME="$home" \
    AWS_REGION="us-east-1" \
    "$@" \
    bash "$script" >"$out" 2>"$err"
}

expect_success(){
  local label="$1" script="$2" home="$3"
  shift 3
  local out="$tmp/$label.out" err="$tmp/$label.err"
  if ! run_uninstall "$script" "$home" "$out" "$err" "$@"; then
    cat "$out" "$err" >&2
    fail "$label should have succeeded"
  fi
}

expect_failure(){
  local label="$1" script="$2" home="$3"
  shift 3
  local out="$tmp/$label.out" err="$tmp/$label.err"
  if run_uninstall "$script" "$home" "$out" "$err" "$@"; then
    cat "$out" "$err" >&2
    fail "$label should have failed closed"
  fi
}

script="$(pwd)/uninstall.sh"
test_home="$tmp/user-home"
mkdir -p "$test_home"

safe_home="$tmp/safe-home"
make_config "$safe_home"
touch "$safe_home/sentinel"
expect_success "custom-home" "$script" "$test_home" "HELLBOX_HOME=$safe_home"
[ ! -e "$safe_home" ] || fail "valid Hellbox home was not removed"

new_home="$tmp/new-home"
legacy_home="$tmp/legacy-home"
make_config "$new_home"
make_config "$legacy_home"
expect_success "legacy-home" "$script" "$test_home" \
  "HELLBOX_HOME=$new_home" "LAMBDADOOM_HOME=$legacy_home"
[ ! -e "$new_home" ] || fail "new Hellbox home was not removed"
[ ! -e "$legacy_home" ] || fail "legacy LambdaDoom home was not removed"

unmarked="$tmp/unmarked"
mkdir -p "$unmarked"
touch "$unmarked/keep"
expect_failure "unmarked-home" "$script" "$test_home" "HELLBOX_HOME=$unmarked"
[ -e "$unmarked/keep" ] || fail "unmarked directory was deleted"

symlink_target="$tmp/symlink-target"
make_config "$symlink_target"
touch "$symlink_target/keep"
ln -s "$symlink_target" "$tmp/symlink-home"
expect_failure "symlink-home" "$script" "$test_home" "HELLBOX_HOME=$tmp/symlink-home"
[ -e "$symlink_target/keep" ] || fail "symlink target was deleted"

make_config "$test_home"
touch "$test_home/keep"
expect_failure "home-directory" "$script" "$test_home" "HELLBOX_HOME=$test_home"
[ -e "$test_home/keep" ] || fail "home directory contents were deleted"

repo_copy="$tmp/repo-copy"
mkdir -p "$repo_copy"
cp uninstall.sh "$repo_copy/uninstall.sh"
make_config "$repo_copy"
touch "$repo_copy/keep"
expect_failure "repo-root" "$repo_copy/uninstall.sh" "$test_home" "HELLBOX_HOME=$repo_copy"
[ -e "$repo_copy/keep" ] || fail "repository root contents were deleted"

parent_root="$tmp/parent-root"
child_repo="$parent_root/repo"
mkdir -p "$child_repo"
cp uninstall.sh "$child_repo/uninstall.sh"
make_config "$parent_root"
touch "$parent_root/keep"
expect_failure "repo-parent" "$child_repo/uninstall.sh" "$test_home" "HELLBOX_HOME=$parent_root"
[ -e "$parent_root/keep" ] || fail "repository parent contents were deleted"

echo "check-uninstall-safety: OK - uninstall local-state deletion is guarded"
