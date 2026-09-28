# Run-RustPaintQemu.ps1 -- boot the freshly built rustupaint.efi in QEMU and
# collect evidence (serial log + framebuffer screenshots), then assert the
# version.
#
# The closed loop, in order:
#   1. build a FAT32 image from qemu_disk/ (mkfatimg.py -- NOT qemu vvfat:
#      vvfat's try_commit aborts on guest delete/cluster-reuse patterns)
#   2. copy it to a per-run temp image, so guest writes (OVMF NvVars) never
#      contaminate the canonical image
#   3. start QEMU with the vendored OVMF, serial redirected to run_logs/, and
#      a QMP socket
#   4. drive the guest over QMP (screendump / sendkey / hover / btn)
#   5. assert APP_VERSION= against expected_version.txt, and refuse to call the
#      run successful if it does not match
#
# Firmware notes (these are load-bearing, do not "fix" them):
#   - No -machine q35: the vendored OVMF build does not boot on q35.
#   - No vars pflash: OVMF then falls back to its built-in UEFI Shell, which
#     runs fs0:\startup.nsh automatically -- that is how the app gets launched.
#   - -usb -device usb-mouse: boot-protocol USB mouse, bound by
#     UsbMouseAbsolutePointerDxe inside firmware/OVMF_CODE.fd. The PS/2 path is
#     dead (SioBusDxe never enumerates PNP0F13) and usb-tablet fails the
#     driver's boot-protocol check.
#
# NOTE: comments in this file are ASCII-only on purpose. See New-BuildVersion.ps1.
param(
  # qmp_drive.py script tokens, e.g. "t3 screendump boot t1 hover|120|300 t1 btn|left t1 screendump drew"
  [string]$Script = 't4 screendump main',
  [int]$MaxShots = 8,
  [string]$ExpectSerial = '',
  # Human-observation mode: SDL window, no automated QMP driving; waits for you
  # to close the window. Use this when you want to actually play with it.
  [switch]$Interactive,
  [int]$MemMb = 512,
  [int]$QmpPort = 4571,
  [string]$OvmfCode = '',
  # Time box for the QMP-driven phase. qmp_drive.py can block indefinitely when
  # the guest never reaches the state its script expects; rather than hang the
  # whole run, it is started detached and this many seconds are allowed to pass
  # before QEMU is killed (which also drops its QMP socket and releases python).
  # Raise it if a script needs longer, e.g. a slow fully-populated boot.
  [int]$TimeoutSec = 45
)

$ErrorActionPreference = 'Stop'

$Root     = Split-Path -Parent $PSScriptRoot
$qemu     = 'C:\Program Files\qemu\qemu-system-x86_64.exe'
$diskDir  = Join-Path $Root 'qemu_disk'
$runLogs  = Join-Path $Root 'run_logs'
$snapshot = Join-Path $Root 'snapshot'

New-Item -ItemType Directory -Force -Path $runLogs, $snapshot | Out-Null

if (!(Test-Path (Join-Path $diskDir 'rustupaint.efi'))) {
  throw "qemu_disk\rustupaint.efi missing -- run tools\Build-RustPaint.ps1 first"
}
if ($OvmfCode -eq '') { $OvmfCode = Join-Path $Root 'firmware\OVMF_CODE.fd' }
if (!(Test-Path $OvmfCode)) { throw "OVMF not found: $OvmfCode" }

$stamp  = Get-Date -Format 'yyyyMMdd_HHmmss'
$serial = Join-Path $runLogs "${stamp}_serial.log"
$stderrLog = Join-Path $runLogs "${stamp}_qemu_stderr.log"
$stdoutLog = Join-Path $runLogs "${stamp}_qemu_stdout.log"

# Unbuffered progress trace.
#
# Write-Host is useless for diagnosing a hang in this script: when the caller
# redirects with `*> file`, the output sits in PowerShell's buffer, so a wedged
# run leaves an almost-empty log (observed twice while chasing the QEMU-launch
# hang -- the file held the first two lines and nothing else, while QEMU was
# happily booting). Add-Content flushes on every call, so
# run_logs/<stamp>_trace.txt shows exactly how far the script got.
$traceFile = Join-Path $runLogs "${stamp}_trace.txt"
function Trace([string]$Text) {
  Add-Content -Path $traceFile -Value ("[{0:HH:mm:ss}] {1}" -f (Get-Date), $Text)
}
Trace "run start (Interactive=$Interactive TargetScript='$Script')"

# ---------------------------------------------------------------- 1. disk image
Write-Host '==> build FAT32 image'
$diskImg = Join-Path $Root 'qemu_disk.img'
python (Join-Path $PSScriptRoot 'mkfatimg.py') create $diskDir $diskImg
if ($LASTEXITCODE -ne 0) { throw "mkfatimg.py failed with exit code $LASTEXITCODE" }
$tempImage = Join-Path $Root "qemu_disk_run.img"
Copy-Item $diskImg $tempImage -Force
Trace 'FAT image built'

# ------------------------------------------------------------------- 2. QEMU
$displayArg = if ($Interactive) { '-display sdl' } else { '-display none' }
$qmpArg     = if ($Interactive) { '' } else { "-qmp tcp:127.0.0.1:$QmpPort,server,nowait " }
$qemuArgs = "-m $MemMb -vga std -net none $displayArg -usb -device usb-mouse -serial file:`"$serial`" " +
  $qmpArg +
  "-drive if=pflash,format=raw,readonly=on,file=`"$OvmfCode`" " +
  "-drive format=raw,file=`"$tempImage`",cache=directsync"

# Launch QEMU. The two modes need genuinely different mechanisms, and three
# other approaches were tried and rejected:
#
#  * Start-Process -- dies on this machine with
#        An item with the same key has already been added. Key: 'PATH'
#    because .NET Framework's ProcessStartInfo.EnvironmentVariables is a
#    case-SENSITIVE dictionary while the ambient environment block carries BOTH
#    "Path" (the Windows default spelling) and "PATH" (a second variable from
#    some `set PATH=...` up the chain). That duplicate is the same thing that
#    breaks EDK2's GenFw step -- see the note in Build-RustPaint.ps1.
#
#  * `cmd /c "start ""t"" /b qemu.exe <args>"` with NO redirect inside -- QEMU
#    inherits cmd's stdout, which is the pipe the CALLER's `*> log` created.
#    PowerShell keeps draining that pipe and `cmd /c` does not return until QEMU
#    exits, so the script blocks forever ON THE LAUNCH LINE: QEMU boots, the
#    serial log looks healthy, and then nothing -- no qmp_drive, no further
#    output, no exit.
#
#  * The same with `> out.log 2> err.log` inside the cmd string -- believed to
#    fix it (cmd opens the files itself, so the child inherits FILE handles
#    rather than the caller's pipe). It did produce the two log files, but the
#    script still never reached the QMP phase, and killing QEMU did not unblock
#    it, so the wait was not on QEMU's handle either. Not trusted; not used.
#
# What is used instead:
#
#  * headless  -> WMI Win32_Process.Create. No inherited handles at all, and it
#    hands back the PID directly. Verified end to end: the run returns 0 and
#    leaves frames in snapshot/. Caveat: the child lives in the WMI service's
#    session (0), which is fine for -display none and useless for SDL.
#
#  * -Interactive -> QEMU runs in the FOREGROUND. Blocking is exactly the
#    desired semantics here ("wait until the user closes the window"), so no
#    launch trickery is needed, and the SDL window appears on the user's desktop
#    because the process belongs to this session. cmd owns the redirection so
#    native stderr never reaches PowerShell (EAP=Stop would fake an exception).
if ($Interactive) {
  Write-Host '==> start QEMU (-display sdl) -- close the window when done'
  Trace "launch QEMU foreground: $qemu"
  $null = cmd /c "`"$qemu`" $qemuArgs > `"$stdoutLog`" 2> `"$stderrLog`""
  Trace 'QEMU exited (interactive)'
  $qemuPid = 0
} else {
  Write-Host '==> start QEMU (-display none)'
  # Clear strays first: without this the PID we pick up could belong to an
  # earlier run, and we would then drive the wrong machine.
  Get-Process -Name 'qemu-system-x86_64' -ErrorAction SilentlyContinue | ForEach-Object {
    $null = cmd /c "taskkill /F /PID $($_.Id) > nul 2>&1"
  }
  $qemuCmdLine = "`"$qemu`" $qemuArgs"
  Trace "launch QEMU via Win32_Process.Create: $qemuCmdLine"
  $created = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
    CommandLine      = $qemuCmdLine
    CurrentDirectory = $Root
  }
  if ($created.ReturnValue -ne 0 -or -not $created.ProcessId) {
    throw "failed to start QEMU (Win32_Process.Create returned $($created.ReturnValue)): $qemuCmdLine"
  }
  $qemuPid = [int]$created.ProcessId
  Write-Host "    qemu pid = $qemuPid"
  Trace "qemu pid = $qemuPid"
  # Give it a moment to come up, or to die immediately (bad args, missing fd).
  Start-Sleep -Seconds 2
  if (!(Get-Process -Id $qemuPid -ErrorAction SilentlyContinue)) {
    throw "QEMU exited immediately after launch (pid $qemuPid): $qemuCmdLine"
  }
}

if (!$Interactive) {
  $py = Join-Path $PSScriptRoot 'qmp_drive.py'
  $pyArgs = @(
    '--port', "$QmpPort",
    '--shot-dir', "`"$snapshot`"",
    '--qemu-pid', "$qemuPid",
    '--max-shots', "$MaxShots",
    '--deadline', "$TimeoutSec"
  )
  if ($Script -ne '') { $pyArgs += @('--script', "`"$Script`"") }
  $pyLog = Join-Path $runLogs "${stamp}_qmp_drive.log"
  Write-Host "    driving guest over QMP (wall-clock budget: ${TimeoutSec}s)"
  # Synchronous, with cmd owning the redirection.
  #
  # This used to be `start /b python ... > log` + Start-Sleep (fire and forget).
  # Two things broke that: (1) the log file was never created, so a python-side
  # failure -- e.g. the missing Pillow import, which killed qmp_drive.py before
  # its first screendump -- was completely invisible; and (2) nothing actually
  # bounded the phase. Running it in the FOREGROUND with qmp_drive.py's own
  # --deadline keeps every failure visible and still guarantees the phase ends.
  # cmd owns the 2>&1 so native stderr never reaches PowerShell (EAP=Stop would
  # turn it into a fake exception); same reasoning as the QEMU launch above.
  Trace 'launch qmp_drive.py'
  $pyCmd = "python `"$py`" " + ($pyArgs -join ' ') + " > `"$pyLog`" 2>&1"
  $null = cmd /c $pyCmd
  Trace 'qmp_drive.py returned'
  if (Test-Path $pyLog) {
    Write-Host "    qmp_drive log: $pyLog"
    Get-Content $pyLog | Out-Host
  } else {
    Write-Host "    WARN: qmp_drive produced no log at $pyLog"
  }
}

# Terminate QEMU with taskkill, not a .NET process handle.
#
# A handle obtained from Get-Process, then .Kill() + .WaitForExit(), can block
# indefinitely in this workspace (observed: the run sat for minutes after the
# QMP phase with QEMU still resident, so the "assert version" step -- and
# therefore the whole script's exit code -- never happened). taskkill /F is
# immediate and cannot block on handle semantics.
if ($qemuPid -ne 0 -and (Get-Process -Id $qemuPid -ErrorAction SilentlyContinue)) {
  Trace "taskkill $qemuPid"
  $null = cmd /c "taskkill /F /PID $qemuPid > nul 2>&1"
  $waited = 0
  while ((Get-Process -Id $qemuPid -ErrorAction SilentlyContinue) -and $waited -lt 40) {
    Start-Sleep -Milliseconds 250
    $waited++
  }
}
Trace 'QEMU stopped'
# qmp_drive does not gate the run: it is time-boxed, and the version assertion
# below is the authoritative pass/fail signal.

# ----------------------------------------------------------------- 3. assert
# QEMU's own stderr is not the evidence: what matters is the serial log plus the
# screendump frames. A QEMU abort still shows up as a missing APP_VERSION= line,
# which the version assertion below turns into a hard failure.

Write-Host '==> assert version'
Trace 'assert version'
$checkArgs = @{
  ExpectedVersionFile = Join-Path $Root 'expected_version.txt'
  SerialLog           = $serial
  SnapshotDir         = $snapshot
}
if ($ExpectSerial -ne '') { $checkArgs.ExpectSerial = $ExpectSerial }
$version = & (Join-Path $PSScriptRoot 'Test-AppVersion.ps1') @checkArgs
Trace 'assert version OK'

Write-Host ""
Write-Host "TRACE:    $traceFile" -ForegroundColor Green
Write-Host "SERIAL:   $serial" -ForegroundColor Green
Write-Host "SNAPSHOT: $snapshot" -ForegroundColor Green
Write-Host "DISK:     $tempImage" -ForegroundColor Green
$version
