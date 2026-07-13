#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Remove Hellbox AWS resources and local state on Windows.

.DESCRIPTION
  The Windows / PowerShell parallel to uninstall.sh. It tears down the MicroVM,
  image, artifact bucket, and CloudFormation stack, then removes ~/.hellbox
  (binary, config, state) and drops that directory from your user PATH.

  Teardown runs through `hellbox destroy --yes` -- the CLI's own SDK-based
  teardown, which reads the standard AWS credential chain (no AWS CLI needed and
  no typed confirmation with --yes). If the binary is gone but a stack is still
  configured, it falls back to the AWS CLI.

  Local-state deletion is guarded the same way uninstall.sh is: it refuses to
  remove a path that is a symlink, your home directory (or a parent of it), the
  repository root (or a parent of it), or any directory lacking a Hellbox config
  marker. Those checks run BEFORE teardown, while config.toml still exists.

  Environment overrides (mirror uninstall.sh):
    HELLBOX_HOME    default $env:USERPROFILE\.hellbox
    HELLBOX_STACK   default Hellbox
    HELLBOX_NAME    default doom
    HELLBOX_BIN     explicit path to the hellbox binary
    AWS_REGION      fallback region when config.toml has none

.EXAMPLE
  ./uninstall.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

function Info($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "warning: $m" -ForegroundColor Yellow }
function Have($n) { [bool](Get-Command $n -ErrorAction SilentlyContinue) }

$HomeDir    = if ($env:HELLBOX_HOME)  { $env:HELLBOX_HOME }  else { Join-Path $env:USERPROFILE '.hellbox' }
$BinDir     = Join-Path $HomeDir 'bin'
$Stack      = if ($env:HELLBOX_STACK) { $env:HELLBOX_STACK } else { 'Hellbox' }
$configPath = Join-Path $HomeDir 'config.toml'
$failed     = $false

# Resolve the hellbox binary: HELLBOX_BIN, the cached copy, then a local build.
$exe = $null
foreach ($c in @($env:HELLBOX_BIN, (Join-Path $BinDir 'hellbox.exe'),
                 (Join-Path $PSScriptRoot 'rs-cli\target\release\hellbox.exe'))) {
  if ($c -and (Test-Path $c -PathType Leaf)) { $exe = $c; break }
}

function Get-ConfigRegion {
  if (Test-Path $configPath) {
    $m = Select-String -Path $configPath -Pattern '^\s*region\s*=\s*"([^"]+)"' | Select-Object -First 1
    if ($m) { return $m.Matches[0].Groups[1].Value }
  }
  if ($env:AWS_REGION)         { return $env:AWS_REGION }
  if ($env:AWS_DEFAULT_REGION) { return $env:AWS_DEFAULT_REGION }
  return 'us-east-1'
}

# Decide whether $dir is a real Hellbox home we are allowed to delete. Run BEFORE
# teardown, so the config-marker check still sees config.toml. $userHome and
# $repoRoot are pre-resolved (TrimEnd '\') paths we refuse to remove or descend
# from; passing them in keeps this guard pure and unit-testable.
function Test-RemovableHome($dir, $userHome, $repoRoot) {
  if (-not (Test-Path $dir)) { return @{ Remove = $false; Reason = 'not found' } }
  $item = Get-Item -LiteralPath $dir -Force
  if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    return @{ Remove = $false; Reason = 'is a symlink/junction' }
  }
  if (-not $item.PSIsContainer) { return @{ Remove = $false; Reason = 'is not a directory' } }

  $resolved = ([IO.Path]::GetFullPath($dir)).TrimEnd('\')
  if ([string]::IsNullOrEmpty($resolved) -or ($resolved -match '^[A-Za-z]:$')) {
    return @{ Remove = $false; Reason = 'resolves to a drive root' }
  }
  if ($userHome) {
    if ($resolved -ieq $userHome) { return @{ Remove = $false; Reason = 'is your home directory' } }
    if ($userHome.StartsWith($resolved + '\', [StringComparison]::OrdinalIgnoreCase)) {
      return @{ Remove = $false; Reason = 'is a parent of your home directory' }
    }
  }
  if ($repoRoot) {
    if ($resolved -ieq $repoRoot) { return @{ Remove = $false; Reason = 'is the repository root' } }
    if ($repoRoot.StartsWith($resolved + '\', [StringComparison]::OrdinalIgnoreCase)) {
      return @{ Remove = $false; Reason = 'is a parent of the repository root' }
    }
  }

  # Marker: a real Hellbox config carries these deploy-written keys.
  $cfg = Join-Path $dir 'config.toml'
  if (-not (Test-Path $cfg)) { return @{ Remove = $false; Reason = 'no Hellbox config marker' } }
  $txt = Get-Content $cfg -Raw
  if (($txt -notmatch '(?m)^\s*artifact_bucket\s*=') -or ($txt -notmatch '(?m)^\s*execution_role_arn\s*=')) {
    return @{ Remove = $false; Reason = 'no Hellbox config marker' }
  }
  return @{ Remove = $true; Reason = '' }
}

$userHomeResolved = ([IO.Path]::GetFullPath($env:USERPROFILE)).TrimEnd('\')
$repoResolved     = if ($PSScriptRoot) { ([IO.Path]::GetFullPath($PSScriptRoot)).TrimEnd('\') } else { '' }
$homeCheck        = Test-RemovableHome $HomeDir $userHomeResolved $repoResolved

# --- 1. Stop a running loopback proxy (`hellbox open`), if any ----------------
try {
  Get-CimInstance Win32_Process -Filter "Name='hellbox.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and ($_.CommandLine -match '\bopen\b') } |
    ForEach-Object {
      Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
      Info "Stopped the running hellbox proxy (pid $($_.ProcessId))"
    }
} catch { }

# --- 2. Tear down AWS ---------------------------------------------------------
# `hellbox destroy` needs a config.toml (it reads the region/account/stack from
# it, and skips gracefully when the stack is already gone). With no config there
# is nothing to tear down, so don't invoke it just to fail.
if ($exe -and (Test-Path $configPath)) {
  Info "Tearing down AWS resources: $exe destroy --yes"
  $env:HELLBOX_HOME = $HomeDir
  & $exe destroy --yes
  if ($LASTEXITCODE -ne 0) { Warn "hellbox destroy failed (exit $LASTEXITCODE)"; $failed = $true }
}
elseif (Test-Path $configPath) {
  # No binary, but a stack may still exist. Fall back to the AWS CLI.
  if (-not (Have aws)) {
    Warn "no hellbox binary and no AWS CLI found -- cannot tear down AWS. Delete the '$Stack' stack manually."
    $failed = $true
  } else {
    $region = Get-ConfigRegion
    Info "Tearing down CloudFormation stack '$Stack' in $region (AWS CLI)"
    $desc = aws cloudformation describe-stacks --region $region --stack-name $Stack 2>&1
    if ($LASTEXITCODE -ne 0) {
      if ("$desc" -match 'does not exist') {
        Info "stack '$Stack' not found in $region -- nothing to delete"
      } else {
        Warn "could not describe stack '$Stack' in ${region}: $desc"; $failed = $true
      }
    } else {
      $bucket = aws cloudformation describe-stacks --region $region --stack-name $Stack `
        --query "Stacks[0].Outputs[?OutputKey=='ArtifactBucket'].OutputValue" --output text 2>$null
      if ($bucket -and $bucket -ne 'None') {
        Info "Emptying artifact bucket: $bucket"
        aws s3 rm "s3://$bucket" --recursive | Out-Null
        if ($LASTEXITCODE -ne 0) { Warn "could not empty s3://$bucket -- delete its objects manually"; $failed = $true }
      }
      Info "Deleting stack '$Stack'"
      aws cloudformation delete-stack --region $region --stack-name $Stack
      if ($LASTEXITCODE -ne 0) {
        Warn "delete-stack failed for '$Stack' -- check the CloudFormation console"; $failed = $true
      } else {
        aws cloudformation wait stack-delete-complete --region $region --stack-name $Stack
        if ($LASTEXITCODE -ne 0) { Warn "stack '$Stack' did not finish deleting -- check the console (resources may remain and still bill)"; $failed = $true }
      }
    }
  }
}
else {
  Info "No config found -- nothing to tear down in AWS."
}

if ($failed) {
  Warn "uninstall finished with errors -- local state was left in place so cleanup can be retried after fixing AWS access."
  Warn "verify in the AWS console that the stack, bucket, and any MicroVM/image are gone (they may still incur cost)."
  exit 1
}

# --- 3. Remove ~/.hellbox (binary, config, state), guarded -------------------
if ($homeCheck.Remove) {
  # Re-confirm it is still a plain directory (not a symlink swapped in since).
  $item = Get-Item -LiteralPath $HomeDir -Force -ErrorAction SilentlyContinue
  if ($item -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -and $item.PSIsContainer) {
    Info "Removing $HomeDir  (binary, config, state)"
    Remove-Item -LiteralPath $HomeDir -Recurse -Force
  }
} elseif (Test-Path $HomeDir) {
  Warn "refusing to remove ${HomeDir}: $($homeCheck.Reason). Remove it by hand if you are sure."
} else {
  Info "Local state not found, skipping: $HomeDir"
}

# --- 4. Drop ~/.hellbox\bin from your user PATH (install.ps1 added it) --------
$normBin  = $BinDir.TrimEnd('\')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath) {
  $kept = ($userPath -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ine $normBin) }) -join ';'
  if ($kept -ne $userPath) {
    [Environment]::SetEnvironmentVariable('Path', $kept, 'User')
    Info "Removed $BinDir from your user PATH (open a new terminal for it to take effect)"
  }
}

Write-Host ''
Info 'Hellbox removed. Delete your clone of the repo if you no longer need it.'
