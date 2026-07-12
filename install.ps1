#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Install the hellbox CLI on Windows.

.DESCRIPTION
  The Windows / PowerShell parallel to deploy.sh's binary-acquisition step:
  resolve your architecture, download the release .exe, verify its SHA256 and
  GitHub build-provenance attestation, cache it under ~/.hellbox/bin, and put
  that directory on your user PATH.

  It stops after installing. Configure AWS credentials, then run `hellbox
  deploy` yourself (that command creates the AWS prerequisites, builds the DOOM
  MicroVM image, launches it, and opens the tab).

  No repo clone is required: the CloudFormation template and the capsule build
  context are baked into the binary, so this script only needs to fetch the exe.

  Environment overrides (these mirror deploy.sh):
    HELLBOX_REPO              default somoore/hellbox
    HELLBOX_VERSION           default latest, or a pinned tag like v1.0.17
    HELLBOX_HOME              default $env:USERPROFILE\.hellbox
    HELLBOX_SKIP_ATTESTATION  set to 1 to skip the attestation check
                              (requires a pinned HELLBOX_VERSION, never latest)

.EXAMPLE
  ./install.ps1

.EXAMPLE
  $env:HELLBOX_VERSION = "v1.0.17"; ./install.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
# The release binary is ~20 MB; without this Invoke-WebRequest renders a
# per-byte progress bar that dominates the download time.
$ProgressPreference = 'SilentlyContinue'

function Info($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "warning: $m" -ForegroundColor Yellow }
function Die($m)  { Write-Host "error: $m" -ForegroundColor Red; exit 1 }
function Have($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }

$Repo    = if ($env:HELLBOX_REPO)    { $env:HELLBOX_REPO }    else { 'somoore/hellbox' }
$Version = if ($env:HELLBOX_VERSION) { $env:HELLBOX_VERSION } else { 'latest' }
$HomeDir = if ($env:HELLBOX_HOME)    { $env:HELLBOX_HOME }    else { Join-Path $env:USERPROFILE '.hellbox' }
$BinDir  = Join-Path $HomeDir 'bin'
$SkipAtt = ($env:HELLBOX_SKIP_ATTESTATION -eq '1')

# --- Target detection -------------------------------------------------------
switch ($env:PROCESSOR_ARCHITECTURE) {
  'AMD64' { $arch = 'x86_64' }
  'ARM64' { $arch = 'arm64' }
  default { Die "unsupported architecture '$($env:PROCESSOR_ARCHITECTURE)' (hellbox ships x86_64 and arm64 Windows builds)" }
}
$asset = "hellbox-windows-$arch.exe"

# --- Resolve the release tag ------------------------------------------------
function Resolve-Tag {
  if ($Version -ne 'latest') { return $Version }
  if (Have gh) {
    $t = gh release view --repo $Repo --json tagName --jq .tagName 2>$null
    if ($LASTEXITCODE -eq 0 -and $t) { return $t.Trim() }
  }
  try {
    $r = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest" `
      -Headers @{ 'User-Agent' = 'hellbox-install' }
    if ($r.tag_name) { return $r.tag_name }
  } catch { }
  Die "could not resolve the latest release for $Repo (set HELLBOX_VERSION to a tag like v1.0.17)"
}

$tag = Resolve-Tag
Info "hellbox $tag  (windows-$arch)"

# --- Attestation gate -------------------------------------------------------
# The .sha256 sidecar ships from the same release as the binary, so it only
# proves the download wasn't truncated or corrupted: an attacker who can swap
# the asset can swap its sidecar too. The cryptographic trust anchor is the
# GitHub build-provenance attestation, bound to the release workflow identity
# and the tag's source ref. To trust nothing prebuilt, build from source
# (cd rs-cli; make release) instead of running this script.
function Confirm-Attestation($file, $releaseTag) {
  if ($SkipAtt) {
    if ($Version -eq 'latest') {
      Die 'HELLBOX_SKIP_ATTESTATION=1 requires a pinned HELLBOX_VERSION, not latest'
    }
    Warn "skipping GitHub artifact attestation verification for pinned release $Version"
    return
  }
  if (-not (Have gh)) {
    Die ("gh (GitHub CLI) is required to verify the build attestation for $asset.`n" +
         "  Install it with 'winget install GitHub.cli', build from source, or set`n" +
         "  HELLBOX_SKIP_ATTESTATION=1 with a pinned HELLBOX_VERSION to bypass.")
  }
  gh attestation verify $file --repo $Repo `
    --signer-workflow "github.com/$Repo/.github/workflows/release.yml" `
    --source-ref "refs/tags/$releaseTag" 2>$null
  if ($LASTEXITCODE -ne 0) {
    Die "GitHub build-provenance attestation verification failed for $asset"
  }
}

function Confirm-Sha256($file, $sumFile) {
  $expected = ((Get-Content $sumFile -Raw).Trim() -split '\s+')[0]
  if (-not $expected) { Die "empty checksum file: $sumFile" }
  $actual = (Get-FileHash $file -Algorithm SHA256).Hash
  if ($actual -ne $expected.ToUpper()) {
    Die "SHA256 mismatch for $(Split-Path $file -Leaf):`n  expected $expected`n  got      $actual"
  }
}

function Get-BinVersion($exePath) {
  try { (& $exePath --version 2>$null).Split()[1] } catch { $null }
}

# --- Download + verify (or re-verify an already-cached match) ---------------
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$exe    = Join-Path $BinDir 'hellbox.exe'
$sum    = Join-Path $BinDir 'hellbox.exe.sha256'
$wanted = $tag.TrimStart('v')

$needDownload = $true
if ((Test-Path $exe) -and (Test-Path $sum) -and ((Get-BinVersion $exe) -eq $wanted)) {
  # Cache hit: still re-verify so a tampered cache can't be silently trusted.
  Confirm-Sha256 $exe $sum
  Confirm-Attestation $exe $tag
  Info "hellbox $tag already installed and verified at $exe"
  $needDownload = $false
}

if ($needDownload) {
  $base   = "https://github.com/$Repo/releases/download/$tag"
  $tmpExe = Join-Path $BinDir $asset
  $tmpSum = "$tmpExe.sha256"
  Info "Downloading $asset"
  Invoke-WebRequest "$base/$asset"        -OutFile $tmpExe
  Invoke-WebRequest "$base/$asset.sha256" -OutFile $tmpSum

  Confirm-Sha256 $tmpExe $tmpSum
  Info "Verified SHA256 for $asset"
  Confirm-Attestation $tmpExe $tag
  if (-not $SkipAtt) { Info "Verified GitHub build-provenance attestation for $asset" }

  Move-Item -Force $tmpExe $exe
  Move-Item -Force $tmpSum $sum
  Info "Installed hellbox $tag to $exe"
}

# --- PATH -------------------------------------------------------------------
# Persist for future terminals (user PATH) and patch this session so `hellbox`
# works immediately without reopening the shell.
$normalizedBin = $BinDir.TrimEnd('\')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$alreadyUserPath = ($userPath -split ';' | Where-Object { $_.TrimEnd('\') -ieq $normalizedBin }).Count -gt 0
if (-not $alreadyUserPath) {
  $newPath = if ([string]::IsNullOrWhiteSpace($userPath)) { $BinDir } else { "$($userPath.TrimEnd(';'));$BinDir" }
  [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
  Info "Added $BinDir to your user PATH"
}
if (($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $normalizedBin }).Count -eq 0) {
  $env:Path = "$($env:Path.TrimEnd(';'));$BinDir"
}

# --- Next steps -------------------------------------------------------------
Write-Host ''
Info 'hellbox is installed. Next steps:'
Write-Host '  1. Configure AWS credentials (aws configure, SSO, or env vars).'
Write-Host '     hellbox reads the standard AWS credential chain, like the AWS CLI.'
Write-Host '  2. Run:  hellbox deploy'
Write-Host '     Creates the AWS prerequisites, builds the DOOM MicroVM image,'
Write-Host '     launches it, and opens the browser tab (about 4-5 minutes).'
Write-Host ''
Write-Host 'Tip: use current Chrome or Edge for the low-latency H.264 path (WebCodecs).'
Write-Host '     In a fresh terminal `hellbox` is on PATH; in this one it already works.'
