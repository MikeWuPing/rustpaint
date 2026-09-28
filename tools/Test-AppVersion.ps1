# Test-AppVersion.ps1 -- assert the build version against a QEMU serial log.
#
# This is the gate that stops "I ran the app and it looked fine" from passing
# when the app that actually ran was a stale .efi. Rules:
#   - APP_VERSION= must appear in the serial log
#   - the LAST occurrence must equal expected_version.txt, byte for byte
#   - the log must not contain a Rust panic line
#   - -ExpectSerial adds a required substring
# Exits non-zero (throws) with the expected/actual pair and both paths.
#
# NOTE: comments in this file are ASCII-only on purpose. See New-BuildVersion.ps1.
param(
  [Parameter(Mandatory=$true)][string]$ExpectedVersionFile,
  [Parameter(Mandatory=$true)][string]$SerialLog,
  [string]$SnapshotDir = '',
  [string]$ExpectSerial = ''
)
$ErrorActionPreference = 'Stop'

$expected = (Get-Content -Raw $ExpectedVersionFile).Trim()
if (!(Test-Path $SerialLog)) {
  throw "serial log not found: $SerialLog (expected version $expected; snapshot=$SnapshotDir)"
}
$content = Get-Content -Raw $SerialLog

$m = [regex]::Matches($content, 'APP_VERSION=([^\r\n]+)')
if ($m.Count -eq 0) {
  throw "APP_VERSION= not found in $SerialLog`n  expected : $expected`n  snapshot : $SnapshotDir"
}
$last = $m[$m.Count - 1].Groups[1].Value.Trim()
if ($last -ne $expected) {
  throw "version mismatch (stale .efi?)`n  expected : $expected`n  actual   : $last`n  log      : $SerialLog`n  snapshot : $SnapshotDir"
}
if ($m.Count -gt 1) {
  # More than one APP_VERSION= means the image ran more than once; the last one
  # is authoritative, but surfacing the count makes "why twice" visible.
  Write-Host "    note: APP_VERSION= appeared $($m.Count) times; using the last"
}
if ([regex]::IsMatch($content, '\[RustPaint\] PANIC')) {
  throw "serial log contains a Rust panic: $SerialLog"
}
if ($ExpectSerial -ne '' -and $content -notmatch [regex]::Escape($ExpectSerial)) {
  throw "expected serial line missing: '$ExpectSerial' in $SerialLog"
}

$last
