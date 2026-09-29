#Requires -Version 5.1
<#
.SYNOPSIS
  Uninstall winimon and optionally remove its directory from the user PATH.
#>
[CmdletBinding()]
param(
  [string]$InstallDir = (Join-Path $HOME '.local\bin'),
  [switch]$KeepPath
)

$ErrorActionPreference = 'Stop'
$exe = Join-Path $InstallDir 'winimon.exe'
if (Test-Path -LiteralPath $exe) {
  Remove-Item -LiteralPath $exe -Force
  Write-Host "Removed: $exe"
} else {
  Write-Host "Not found (already removed): $exe"
}

if (-not $KeepPath) {
  $current = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not [string]::IsNullOrEmpty($current)) {
    $parts = $current -split ';' | Where-Object { $_ -ne '' -and ($_.TrimEnd('\') -ine $InstallDir.TrimEnd('\')) }
    $newPath = ($parts -join ';')
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Removed from user PATH: $InstallDir"
  }
}
