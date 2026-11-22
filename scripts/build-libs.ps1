<#
  build-libs.ps1 - build the at9codec shared library locally for the host arch and
  package it as per-os/arch release assets.

  The GitHub Actions workflow (.github/workflows/build-libs.yml) builds every
  platform in CI (including macOS, which needs no local Mac). This script is the
  LOCAL path for the two platforms the Windows dev box can build itself:

    - Windows  : native `cargo build` -> at9codec.dll (+ at9codec.dll.lib)
    - Linux    : `cargo build` inside a WSL Ubuntu* distro -> libat9codec.so

  Run it on the x64 PC to get the x64 assets, or on a Windows-ARM machine to get
  the arm64 assets (Nuitka-style: you build for the arch you run on).

  Output: dist/at9codec-<os>-<arch>.zip, each containing the library, the C header
  and LICENSE (+ the import lib on Windows) - the exact format the embedding app's
  scripts/update_native_libs.py fetches.

  Releases are made MANUALLY (you create the GitHub release and write its
  description by hand). This script only UPLOADS the lib assets, and only when you
  pass -Tag: it refuses to create or overwrite a release, so the release body is
  never clobbered.

  USAGE
    pwsh scripts/build-libs.ps1                      # build win+linux -> dist/
    pwsh scripts/build-libs.ps1 -Targets windows     # just Windows
    pwsh scripts/build-libs.ps1 -Tag libs-latest     # build, then upload to an
                                                      # EXISTING release 'libs-latest'
  FLAGS
    -Targets windows,linux   which to build (default both)
    -Tag <tag>               upload the zips to this existing release (assets only)
    -WslDistro <name>        force a specific WSL distro (default: auto-detect Ubuntu*)
    -SkipLinux               alias for -Targets windows
    -X86                     also build 32-bit (i686) Windows/Linux libs (cross-compiled
                             from the x64 host; Linux needs gcc-multilib, auto-installed)
#>
[CmdletBinding()]
param(
  [string[]]$Targets = @('windows', 'linux'),
  [string]$Tag = '',
  [string]$WslDistro = '',
  [switch]$SkipLinux,
  [switch]$X86
)

$ErrorActionPreference = 'Stop'
if ($SkipLinux) { $Targets = $Targets | Where-Object { $_ -ne 'linux' } }

$RepoRoot = Split-Path -Parent $PSScriptRoot
$DistDir  = Join-Path $RepoRoot 'dist'
New-Item -ItemType Directory -Force $DistDir | Out-Null

switch ($env:PROCESSOR_ARCHITECTURE) {
  'AMD64' { $archTag = 'x64' }
  'ARM64' { $archTag = 'arm64' }
  default { throw "Unsupported host arch: $($env:PROCESSOR_ARCHITECTURE)" }
}
Write-Host "==== at9codec build-libs  host-arch=$archTag  targets=$($Targets -join ',') ====" -ForegroundColor Cyan

# --- helpers ---------------------------------------------------------------

function Resolve-WslDistro {
  # Explicit -WslDistro wins; else a distro named 'Ubuntu', else first 'Ubuntu*'.
  # `wsl -l -q` emits UTF-16, so the console encoding must be set to read it.
  if ($WslDistro) { return $WslDistro }
  $prev = [Console]::OutputEncoding
  try {
    [Console]::OutputEncoding = [System.Text.Encoding]::Unicode
    $raw = wsl -l -q 2>$null
  } catch { return $null } finally { [Console]::OutputEncoding = $prev }
  $d = @($raw) | ForEach-Object { ($_ -replace '[^0-9A-Za-z._-]', '').Trim() } | Where-Object { $_ }
  if ($d -contains 'Ubuntu') { return 'Ubuntu' }
  $u = $d | Where-Object { $_ -like 'Ubuntu*' } | Select-Object -First 1
  if ($u) { return $u }
  return $null
}

function Package-Tag([string]$tag, [string[]]$files) {
  # Zip the given files flat into dist/at9codec-<tag>.zip (overwrites).
  $zip = Join-Path $DistDir "at9codec-$tag.zip"
  if (Test-Path $zip) { Remove-Item $zip -Force }
  Compress-Archive -Path $files -DestinationPath $zip -CompressionLevel Optimal
  Write-Host "  packaged $([IO.Path]::GetFileName($zip))" -ForegroundColor Green
  return $zip
}

$built = New-Object System.Collections.Generic.List[string]
$header  = Join-Path $RepoRoot 'include/at9codec.h'
$license = Join-Path $RepoRoot 'LICENSE'

# --- Windows (native) ------------------------------------------------------

if ($Targets -contains 'windows') {
  Write-Host "windows-${archTag}: cargo build --release -p at9codec" -ForegroundColor Yellow
  Push-Location $RepoRoot
  try { cargo build --release -p at9codec; if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" } }
  finally { Pop-Location }
  $rel = Join-Path $RepoRoot 'target/release'
  $files = @((Join-Path $rel 'at9codec.dll'), $header, $license)
  $implib = Join-Path $rel 'at9codec.dll.lib'
  if (Test-Path $implib) { $files += $implib }   # import lib for C/C++ linking
  foreach ($f in $files) { if (-not (Test-Path $f)) { throw "missing build output: $f" } }
  $built.Add((Package-Tag "windows-$archTag" $files))

  # Optional 32-bit (i686) Windows lib, cross-compiled from the x64 host.
  if ($X86 -and $archTag -eq 'x64') {
    Write-Host "windows-x86: cargo build --release --target i686-pc-windows-msvc" -ForegroundColor Yellow
    Push-Location $RepoRoot
    try {
      rustup target add i686-pc-windows-msvc 2>&1 | Out-Null
      cargo build --release --target i686-pc-windows-msvc -p at9codec
      if ($LASTEXITCODE -ne 0) { throw "windows-x86 build failed ($LASTEXITCODE)" }
    } finally { Pop-Location }
    $rel86 = Join-Path $RepoRoot 'target/i686-pc-windows-msvc/release'
    $files86 = @((Join-Path $rel86 'at9codec.dll'), $header, $license)
    $il86 = Join-Path $rel86 'at9codec.dll.lib'
    if (Test-Path $il86) { $files86 += $il86 }
    foreach ($f in $files86) { if (-not (Test-Path $f)) { throw "missing x86 build output: $f" } }
    $built.Add((Package-Tag "windows-x86" $files86))
  }
}

# --- Linux (via WSL) -------------------------------------------------------

if ($Targets -contains 'linux') {
  $distro = Resolve-WslDistro
  if (-not $distro) {
    Write-Warning "linux: no WSL Ubuntu* distro found - skipped (install one or pass -WslDistro)"
  } else {
    $drive   = $RepoRoot.Substring(0, 1).ToLower()
    $sub     = ($RepoRoot.Substring(2) -replace '\\', '/').TrimStart('/')
    $wslRepo = "/mnt/$drive/$sub"
    $stage   = Join-Path $DistDir 'stage-linux'
    New-Item -ItemType Directory -Force $stage | Out-Null

    # Bash build script: install toolchain if missing, sync sources into the ext4
    # fs (fast + avoids /mnt file-lock quirks), cargo build, copy the .so back.
    $L = New-Object System.Collections.Generic.List[string]
    $L.Add('#!/bin/bash'); $L.Add('set -e'); $L.Add('set -u')
    $L.Add('REPO="$1"')
    $L.Add('if ! command -v rsync >/dev/null || ! command -v cc >/dev/null || ! command -v curl >/dev/null; then')
    $L.Add('  export DEBIAN_FRONTEND=noninteractive; apt-get update -qq; apt-get install -y build-essential curl rsync')
    $L.Add('fi')
    $L.Add('export RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo')
    $L.Add('if [ ! -x "$CARGO_HOME/bin/cargo" ] && ! command -v cargo >/dev/null; then')
    $L.Add('  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path')
    $L.Add('fi')
    $L.Add('export PATH="$CARGO_HOME/bin:$PATH"')
    $L.Add('mkdir -p /root/at9codec')
    $L.Add('rsync -a --delete --exclude .git --exclude target --exclude dist "$REPO/" /root/at9codec/')
    $L.Add('X86="$2"')
    $L.Add('cd /root/at9codec')
    $L.Add('cargo build --release -p at9codec')
    $L.Add('cp target/release/libat9codec.so "$REPO/dist/stage-linux/libat9codec.so"')
    # Optional 32-bit (i686) build: needs the 32-bit C runtime/linker + the target.
    $L.Add('if [ "$X86" = "yes" ]; then')
    $L.Add('  command -v i686-linux-gnu-gcc >/dev/null || apt-get install -y gcc-multilib')
    $L.Add('  rustup target add i686-unknown-linux-gnu')
    $L.Add('  cargo build --release --target i686-unknown-linux-gnu -p at9codec')
    $L.Add('  cp target/i686-unknown-linux-gnu/release/libat9codec.so "$REPO/dist/stage-linux/libat9codec-x86.so"')
    $L.Add('fi')
    $L.Add('echo BUILD_EXIT=0')
    $scriptPath = Join-Path $stage 'build-linux-lib.sh'
    [System.IO.File]::WriteAllText($scriptPath, (($L -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))

    $x86flag = if ($X86 -and $archTag -eq 'x64') { 'yes' } else { 'no' }
    Write-Host "linux-${archTag}: building via WSL distro '$distro' ..." -ForegroundColor Yellow
    wsl -d $distro -u root -- bash -c "chmod +x '$wslRepo/dist/stage-linux/build-linux-lib.sh'; '$wslRepo/dist/stage-linux/build-linux-lib.sh' '$wslRepo' '$x86flag'"
    $so = Join-Path $stage 'libat9codec.so'
    if (-not (Test-Path $so)) { throw "linux build produced no libat9codec.so (see output above)" }
    $built.Add((Package-Tag "linux-$archTag" @($so, $header, $license)))
    if ($x86flag -eq 'yes') {
      $so86 = Join-Path $stage 'libat9codec-x86.so'
      if (-not (Test-Path $so86)) { throw "linux-x86 build produced no libat9codec-x86.so" }
      # Package under the plain lib name inside the x86 zip.
      $so86named = Join-Path $stage 'x86/libat9codec.so'
      New-Item -ItemType Directory -Force (Split-Path $so86named) | Out-Null
      Copy-Item $so86 $so86named -Force
      $built.Add((Package-Tag "linux-x86" @($so86named, $header, $license)))
    }
  }
}

# --- optional upload (assets only, never create/overwrite a release) --------

if ($Tag) {
  if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw "gh CLI not found - needed for -Tag upload" }
  & gh release view $Tag *> $null
  if ($LASTEXITCODE -ne 0) {
    throw "release '$Tag' does not exist. Create it (with its description) on GitHub first, then re-run with -Tag $Tag. This script never creates or overwrites a release."
  }
  foreach ($z in $built) {
    Write-Host "uploading $([IO.Path]::GetFileName($z)) -> release $Tag" -ForegroundColor Yellow
    & gh release upload $Tag $z --clobber
    if ($LASTEXITCODE -ne 0) { throw "gh release upload failed for $z" }
  }
  Write-Host "uploaded $($built.Count) asset(s) to release $Tag (release body untouched)" -ForegroundColor Green
}

Write-Host "`nDone. Assets in $DistDir :" -ForegroundColor Cyan
$built | ForEach-Object { Write-Host "  $_" }
if (-not $Tag) { Write-Host "Upload them to a release you created by hand, or re-run with -Tag <tag>." -ForegroundColor DarkGray }
