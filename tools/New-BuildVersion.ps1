# New-BuildVersion.ps1 -- bump VERSION.txt and regenerate Version.h.
#
# Contract (mirrors the workspace convention):
#   - VERSION.txt holds two lines: VERSION=major.minor.patch / BUILD=<int>
#   - every build bumps BUILD by one and writes it back
#   - the version string is  <ver>.<build>+<yyyyMMdd_HHmmss>
#     e.g. 0.1.0.7+20260927_204100
#   - that exact string is what UefiMain.c prints as APP_VERSION=, what the
#     status bar draws, and what expected_version.txt holds
#
# NOTE: comments in this file are ASCII-only on purpose. Windows PowerShell
# 5.1 parses a .ps1 that has no BOM as ANSI(GBK); UTF-8 Chinese comments then
# get mis-decoded, swallow the trailing LF of a line, and break the parser
# (the file silently reads short and braces stop matching). Keep it ASCII.
param(
  [Parameter(Mandatory=$true)][string]$ProjectRoot,
  [Parameter(Mandatory=$true)][string]$OutputHeader
)
$ErrorActionPreference = 'Stop'

$versionFile = Join-Path $ProjectRoot 'VERSION.txt'
if (!(Test-Path $versionFile)) {
  Set-Content -Path $versionFile -Value "VERSION=0.1.0`nBUILD=0"
}

$lines = Get-Content $versionFile
$verLine = $lines | Where-Object { $_ -match '^VERSION=' } | Select-Object -First 1
$bldLine = $lines | Where-Object { $_ -match '^BUILD=' } | Select-Object -First 1
if (!$verLine -or !$bldLine) { throw "VERSION.txt must hold VERSION= and BUILD= lines: $versionFile" }

$version = $verLine.Substring(8).Trim()
$build = [int]$bldLine.Substring(6).Trim()
$build = $build + 1
Set-Content -Path $versionFile -Value ("VERSION=$version`nBUILD=$build")

$parts = $version.Split('.')
if ($parts.Count -ne 3) { throw "VERSION must be major.minor.patch: $version" }

$timestamp = Get-Date -Format 'yyyyMMdd_HHmmss'
$versionString = "$version.$build+$timestamp"

$template = Get-Content -Raw (Join-Path $PSScriptRoot 'Version.h.template')
$header = $template.Replace('@VER_MAJOR@', $parts[0]).
  Replace('@VER_MINOR@', $parts[1]).
  Replace('@VER_PATCH@', $parts[2]).
  Replace('@BUILD_NUMBER@', [string]$build).
  Replace('@BUILD_TIMESTAMP@', $timestamp).
  Replace('@VERSION_STRING@', $versionString)

# ASCII, no BOM: Version.h is included by MSVC sources and is pure ASCII.
Set-Content -Path $OutputHeader -Value $header -Encoding ASCII

[pscustomobject]@{
  Version       = $version
  Build         = $build
  Timestamp     = $timestamp
  VersionString = $versionString
  Header        = $OutputHeader
}
