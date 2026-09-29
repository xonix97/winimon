#Requires -Version 5.1
<#
.SYNOPSIS
  Build + install winimon and automatically add it to the user PATH.
#>
[CmdletBinding()]
param(
  [string]$InstallDir = (Join-Path $HOME '.local\bin')
)

$ErrorActionPreference = 'Stop'

function Add-DirToUserPath([string]$Dir) {
  $current = [Environment]::GetEnvironmentVariable('Path', 'User')
  if ([string]::IsNullOrEmpty($current)) { $current = '' }
  $parts = $current -split ';' | Where-Object { $_ -ne '' }
  $already = $false
  foreach ($p in $parts) {
    if ($p.TrimEnd('\') -ieq $Dir.TrimEnd('\')) { $already = $true; break }
  }
  if (-not $already) {
    $newPath = if ($current -eq '' -or $current.EndsWith(';')) { "$current$Dir" } else { "$current;$Dir" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added to user PATH: $Dir"
  } else {
    Write-Host "Already in user PATH: $Dir"
  }
  if (($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $Dir.TrimEnd('\') }).Count -eq 0) {
    $env:Path = "$env:Path;$Dir"
  }
}

Write-Host 'Building winimon (release)...'
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed with exit code $LASTEXITCODE" }

$src = Join-Path $PSScriptRoot 'target\release\winimon.exe'
if (-not (Test-Path -LiteralPath $src)) { throw "Expected binary not found: $src" }

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
$dst = Join-Path $InstallDir 'winimon.exe'
Copy-Item -LiteralPath $src -Destination $dst -Force
Write-Host "Installed: $dst"

Add-DirToUserPath $InstallDir

Write-Host ''
Write-Host 'Verifying...'
& $dst --version
Write-Host ''
Write-Host "Done. If 'winimon' is not recognized in open terminals, restart the terminal (user PATH was updated)."
