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
# On 32-bit PowerShell running on 64-bit Windows (WOW64), PROCESSOR_ARCHITECTURE
# reports x86 while PROCESSOR_ARCHITEW6432 holds the real native arch. Prefer the
# native one so we don't wrongly reject a machine that can run the 64-bit build.
$nativeArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
switch ($nativeArch) {
  'AMD64' { $arch = 'x86_64' }
  'ARM64' { $arch = 'arm64' }
  default { Die "unsupported architecture '$nativeArch' (hellbox ships x86_64 and arm64 Windows builds)" }
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
# Both verifiers return $true/$false rather than exiting, so a bad *cache* can
# fall through to a fresh download while a bad *download* is a hard Die at the
# call site. Neither one runs the binary - the cache is trusted only after these
# pass, so `install.ps1` never executes an unverified exe (e.g. one left by a
# manual install or a tampered cache).
function Test-Attestation($file, $releaseTag) {
  if ($SkipAtt) {
    if ($Version -eq 'latest') {
      Die 'HELLBOX_SKIP_ATTESTATION=1 requires a pinned HELLBOX_VERSION, not latest'
    }
    Warn "skipping GitHub artifact attestation verification for pinned release $Version"
    return $true
  }
  if (-not (Have gh)) {
    Die ("gh (GitHub CLI) is required to verify the build attestation for $asset.`n" +
         "  Install it with 'winget install GitHub.cli', build from source, or set`n" +
         "  HELLBOX_SKIP_ATTESTATION=1 with a pinned HELLBOX_VERSION to bypass.")
  }
  # --source-ref binds the check to this exact tag, so an older artifact from the
  # same workflow (or a swapped asset+sidecar) fails rather than passing.
  gh attestation verify $file --repo $Repo `
    --signer-workflow "github.com/$Repo/.github/workflows/release.yml" `
    --source-ref "refs/tags/$releaseTag" 2>$null
  return ($LASTEXITCODE -eq 0)
}

function Test-Sha256($file, $sumFile) {
  $expected = ((Get-Content $sumFile -Raw).Trim() -split '\s+')[0]
  if (-not $expected) { Die "empty checksum file: $sumFile" }
  $actual = (Get-FileHash $file -Algorithm SHA256).Hash
  return ($actual -eq $expected.ToUpper())
}

# Download a release asset. Falls back to `gh release download` when the public
# URL fails, so a private HELLBOX_REPO (whose tag Resolve-Tag already found via
# gh) still installs - matching deploy.sh's authenticated-download fallback.
function Get-ReleaseAsset($assetName, $outFile) {
  $url = "https://github.com/$Repo/releases/download/$tag/$assetName"
  try {
    Invoke-WebRequest $url -OutFile $outFile
    return
  } catch {
    if (Have gh) {
      Info "public download failed for $assetName - fetching via gh (private repo?)"
      gh release download $tag --repo $Repo --pattern $assetName --output $outFile --clobber 2>$null
      if ($LASTEXITCODE -eq 0 -and (Test-Path $outFile)) { return }
    }
    Die "could not download $assetName from $Repo ($tag): $($_.Exception.Message)"
  }
}

# --- Download + verify (or re-verify an already-cached match) ---------------
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$exe = Join-Path $BinDir 'hellbox.exe'
$sum = Join-Path $BinDir 'hellbox.exe.sha256'

$needDownload = $true
if ((Test-Path $exe) -and (Test-Path $sum)) {
  # Re-verify the cache BEFORE trusting or running it. SHA256 first (integrity),
  # then attestation (provenance). A normal attestation is bound to $tag, so a
  # pass also proves the cache is *this* release. But when HELLBOX_SKIP_ATTESTATION
  # is set, Test-Attestation is a no-op, so nothing ties the cache to $tag --
  # confirm the version explicitly (only after SHA256 passed, so we're not running
  # an unverified binary). An older or tampered cache falls through to a fresh download.
  $cacheOk = (Test-Sha256 $exe $sum) -and (Test-Attestation $exe $tag)
  if ($cacheOk -and $SkipAtt) {
    $cachedVer = try { (& $exe --version 2>$null).Split()[1] } catch { $null }
    if ($cachedVer -ne $tag.TrimStart('v')) { $cacheOk = $false }
  }
  if ($cacheOk) {
    Info "hellbox $tag already installed and verified at $exe"
    $needDownload = $false
  } else {
    Warn "cached hellbox failed verification or is a different release - re-downloading"
  }
}

if ($needDownload) {
  $tmpExe = Join-Path $BinDir $asset
  $tmpSum = "$tmpExe.sha256"
  Info "Downloading $asset"
  Get-ReleaseAsset $asset          $tmpExe
  Get-ReleaseAsset "$asset.sha256" $tmpSum

  if (-not (Test-Sha256 $tmpExe $tmpSum)) {
    $expected = ((Get-Content $tmpSum -Raw).Trim() -split '\s+')[0]
    $actual   = (Get-FileHash $tmpExe -Algorithm SHA256).Hash
    Die "SHA256 mismatch for ${asset}:`n  expected $expected`n  got      $actual"
  }
  Info "Verified SHA256 for $asset"
  if (-not (Test-Attestation $tmpExe $tag)) {
    Die "GitHub build-provenance attestation verification failed for $asset"
  }
  if (-not $SkipAtt) { Info "Verified GitHub build-provenance attestation for $asset" }

  Move-Item -Force $tmpExe $exe
  Move-Item -Force $tmpSum $sum
  Info "Installed hellbox $tag to $exe"
}

# --- PATH -------------------------------------------------------------------
# Prepend, not append: if a stale hellbox already sits earlier on PATH (an old
# manual copy, say), appending would let the advertised `hellbox deploy` keep
# invoking that one. Prepending makes the binary we just verified win - both for
# future terminals (user PATH) and this session.
$normalizedBin = $BinDir.TrimEnd('\')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$alreadyUserPath = ($userPath -split ';' | Where-Object { $_.TrimEnd('\') -ieq $normalizedBin }).Count -gt 0
if (-not $alreadyUserPath) {
  $newPath = if ([string]::IsNullOrWhiteSpace($userPath)) { $BinDir } else { "$BinDir;$($userPath.TrimEnd(';'))" }
  [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
  Info "Added $BinDir to the front of your user PATH"
}
# This session: drop any existing entry, then prepend, so `hellbox` resolves here now.
$env:Path = "$BinDir;" + (($env:Path -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ine $normalizedBin) }) -join ';')

# Warn if another hellbox is installed elsewhere. We prepend to *user* PATH, but
# new processes build PATH as machine-entries-then-user-entries, so a hellbox on
# the MACHINE PATH still shadows ours in fresh terminals (this session is fine --
# we patched it above). Call that out specifically, since the fix differs.
$other = Get-Command hellbox -All -ErrorAction SilentlyContinue |
  Where-Object { $_.Source -and ($_.Source.TrimEnd('\') -ine $exe.TrimEnd('\')) } |
  Select-Object -First 1
if ($other) {
  $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
  $otherDir = (Split-Path $other.Source -Parent).TrimEnd('\')
  $onMachine = ($machinePath -split ';' | Where-Object { $_.TrimEnd('\') -ieq $otherDir }).Count -gt 0
  if ($onMachine) {
    Warn "another hellbox is on the machine PATH at $($other.Source). New terminals resolve machine PATH before your user PATH, so they will run that one, not the copy just installed. Remove it, or invoke this build by full path: $exe"
  } else {
    Warn "another hellbox is on PATH at $($other.Source). This session and new terminals prefer the copy just installed; remove that one if you want it gone."
  }
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
