#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Remove Hellbox AWS resources and local state on Windows.

.DESCRIPTION
  The Windows / PowerShell parallel to uninstall.sh. It removes ~/.hellbox
  (binary, config, state) and drops that directory from your user PATH. It can
  ALSO tear down your AWS resources (MicroVM, image, artifact bucket, and
  CloudFormation stack), but only after you confirm: the default is to keep
  everything in AWS. Removing the CLI never silently deletes cloud resources.

  When you confirm, teardown runs through `hellbox destroy --yes` -- the CLI's
  own SDK-based teardown, which reads the standard AWS credential chain (no AWS
  CLI needed). If the binary is gone but a stack is still configured, it falls
  back to the AWS CLI. Set $env:HELLBOX_YES = '1' to confirm non-interactively
  (for scripts); a non-interactive shell without it keeps AWS resources.

  Local-state deletion is guarded the same way uninstall.sh is: it refuses to
  remove a path that is a symlink, your home directory (or a parent of it), the
  repository root (or a parent of it), or any directory lacking a Hellbox config
  marker. Those checks run BEFORE teardown, while config.toml still exists.

  Environment overrides (mirror uninstall.sh):
    HELLBOX_HOME    default $env:USERPROFILE\.hellbox
    HELLBOX_STACK   default Hellbox
    HELLBOX_NAME    default doom
    HELLBOX_BIN     explicit path to the hellbox binary
    HELLBOX_YES     set to 1 to confirm AWS teardown non-interactively
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

$newHome    = Join-Path $env:USERPROFILE '.hellbox'
$legacyHome = if ($env:LAMBDADOOM_HOME) { $env:LAMBDADOOM_HOME } else { Join-Path $env:USERPROFILE '.lambdadoom' }
$HomeDir    = if ($env:HELLBOX_HOME) {
  $env:HELLBOX_HOME
} elseif ((-not (Test-Path (Join-Path $newHome 'config.toml'))) -and (Test-Path (Join-Path $legacyHome 'config.toml'))) {
  $legacyHome
} else {
  $newHome
}
$isLegacy   = ($HomeDir -ieq $legacyHome)
$BinDir     = Join-Path $HomeDir 'bin'
$Stack      = if ($env:HELLBOX_STACK) { $env:HELLBOX_STACK } elseif ($isLegacy) { 'LambdaDoom' } else { 'Hellbox' }
$Name       = if ($env:HELLBOX_NAME) { $env:HELLBOX_NAME } elseif ($env:LAMBDADOOM_NAME) { $env:LAMBDADOOM_NAME } else { 'doom' }
$configPath = Join-Path $HomeDir 'config.toml'
$failed     = $false

# Resolve the CLI from explicit overrides, cache, PATH, then a local build.
$exe = $null
$pathHellbox = Get-Command hellbox.exe -ErrorAction SilentlyContinue | Select-Object -First 1
$pathLegacy = Get-Command ldoom.exe -ErrorAction SilentlyContinue | Select-Object -First 1
foreach ($c in @($env:HELLBOX_BIN, $env:LDOOM_BIN, $env:LAMBDADOOM_BIN,
                 (Join-Path $BinDir 'hellbox.exe'), (Join-Path $BinDir 'ldoom.exe'),
                 $pathHellbox.Source, $pathLegacy.Source,
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
# AWS teardown is opt-in and confirmed. Uninstalling the CLI must never silently
# delete a user's cloud resources. Default is to KEEP everything in AWS; the user
# (or $env:HELLBOX_YES = '1' for scripts) has to say yes.
$removeAws = $false
if (Test-Path $configPath) {
  if ($env:HELLBOX_YES -eq '1') {
    $removeAws = $true
    Info "HELLBOX_YES=1 set: will remove AWS resources (MicroVM, image, bucket, stack)."
  }
  elseif ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
    Write-Host ""
    Write-Host "This can also delete your Hellbox AWS resources:" -ForegroundColor Yellow
    Write-Host "  - the DOOM MicroVM and its image"
    Write-Host "  - the CloudFormation stack ('$Stack') and its S3 artifact bucket"
    Write-Host "These live in YOUR AWS account. Deleting them is irreversible."
    $reply = Read-Host "Remove them now? [y/N]"
    if ($reply -match '^(y|yes)$') { $removeAws = $true }
    else { Info "Keeping all AWS resources. Run 'hellbox destroy' (or re-run with HELLBOX_YES=1) to remove them later." }
  }
  else {
    Info "Non-interactive shell: keeping AWS resources. Set HELLBOX_YES=1 to remove them, or run 'hellbox destroy'."
  }
}

# `hellbox destroy` needs a config.toml (it reads the region/account/stack from
# it, and skips gracefully when the stack is already gone). With no config there
# is nothing to tear down, so don't invoke it just to fail.
if ($removeAws -and $exe -and (Test-Path $configPath)) {
  Info "Tearing down AWS resources: $exe destroy --name $Name --yes"
  $env:HELLBOX_HOME = $HomeDir
  & $exe destroy --name $Name --yes
  if ($LASTEXITCODE -ne 0) { Warn "hellbox destroy failed (exit $LASTEXITCODE)"; $failed = $true }
}
elseif ($removeAws -and (Test-Path $configPath)) {
  # Fail closed without the CLI. The CLI verifies stack ownership and removes
  # MicroVM/image state before deleting CloudFormation resources; an AWS CLI
  # fallback cannot safely reproduce those guarantees from editable local config.
  Warn "no hellbox/ldoom binary found -- refusing AWS teardown because stack ownership and capsule cleanup cannot be verified."
  Warn "reinstall the CLI or put it on PATH, then retry; local state was preserved."
  $failed = $true
}
elseif (-not (Test-Path $configPath)) {
  Info "No config found -- nothing to tear down in AWS."
}
# else: config exists but the user declined teardown; the prompt block above
# already said so, so stay quiet here.

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
  exit 1
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
