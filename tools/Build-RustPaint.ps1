# Build-RustPaint.ps1 -- single entry point for building rustupaint.efi.
#
# Four stages, in this order:
#   1. version bump  -> Version.h + expected_version.txt + rust/version.rs.txt
#   2. cargo build   -> rust/target/x86_64-unknown-uefi/release/<static lib>
#   3. normalize     -> copy that static lib to
#                       RustPaintPkg/Application/RustPaint/rp_core.lib,
#                       which is where RustPaint.inf's /LIBPATH:$(MODULE_DIR)
#                       expects it
#   4. EDK2 build    -> rustupaint.efi, then copy to dist/ and qemu_disk/
#
# Stage 2 must run before stage 4: the EDK2 link step consumes the .lib, so a
# stale .lib silently links old Rust code (the same class of failure as
# running a stale .efi -- see tools/Test-AppVersion.ps1).
#
# NOTE: comments in this file are ASCII-only on purpose. See the note in
# New-BuildVersion.ps1 for why (PowerShell 5.1 + BOM-less .ps1 + non-ASCII).
param(
  [ValidateSet("DEBUG", "RELEASE", "NOOPT")][string]$Target = "DEBUG",
  [switch]$SkipRust,
  [switch]$SkipEdk2,
  [switch]$Clean,
  # Build tools/_ruststub.c instead of the Rust library. Lets the C side, the
  # EDK2 package, the /DEFAULTLIB: link mechanism and the QEMU loop all be
  # verified before a Rust toolchain exists. The resulting .efi reports
  # "(stub)" on the serial channel and deliberately draws no UI.
  [switch]$StubRust,
  # Archive the EDK2 build output here. Use this instead of piping the
  # script's own stdout through Tee/Select at the call site: a caller-side
  # pipeline can kill the build mid-flight and leave a process holding the
  # build directory (observed twice in this workspace). The script tees
  # internally, after capturing, so the console still gets everything.
  [string]$LogFile = ''
)

$ErrorActionPreference = 'Stop'

$Root      = Split-Path -Parent $PSScriptRoot
$RustDir   = Join-Path $Root 'rust'
$ModuleDir = Join-Path $Root 'RustPaintPkg\Application\RustPaint'
$Edk2      = 'D:\Work\Code\edk2'
$Header    = Join-Path $ModuleDir 'Version.h'
$RunLogs   = Join-Path $Root 'run_logs'

function Write-Step([string]$Text) { Write-Host "==> $Text" -ForegroundColor Cyan }

# ---------------------------------------------------------------- toolchain env
# Repair INCLUDE before invoking the EDK2 build.
#
# Why this is needed: MSVC's own include dir carries only the compiler-provided
# headers (stdint.h, vcruntime.h, sal.h, ...). The C standard library headers
# LVGL needs -- <stddef.h>, <stdlib.h>, <string.h> -- live in the **Windows SDK
# UCRT** include dir. Normally vcvarsall.bat discovers the SDK version by
# querying the registry; when that query cannot run (a locked-down or sandboxed
# shell), INCLUDE ends up with the MSVC dir only and every LVGL translation unit
# dies with:
#     lvgl/src/misc/lv_types.h(20): fatal error C1083: cannot open stddef.h
# Resolving the directories here -- by directory enumeration, no registry -- and
# setting INCLUDE *after* edksetup.bat (which is what vcvarsall runs inside)
# makes the build independent of that query. Correctness is checked below: the
# chosen UCRT dir must actually contain stddef.h.
function Resolve-MsvcInclude {
  $base = 'C:\Program Files (x86)\Microsoft Visual Studio\2019\Professional\VC\Tools\MSVC'
  $dirs = Get-ChildItem $base -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName 'include\vcruntime.h') } |
    Sort-Object Name -Descending
  if ($dirs.Count -eq 0) { throw "no usable MSVC toolset under $base" }
  return (Join-Path $dirs[0].FullName 'include')
}

function Resolve-SdkInclude {
  $base = 'C:\Program Files (x86)\Windows Kits\10\Include'
  $dirs = Get-ChildItem $base -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName 'ucrt\stddef.h') } |
    Sort-Object Name -Descending
  if ($dirs.Count -eq 0) { throw "no Windows SDK with ucrt\stddef.h under $base" }
  $root = $dirs[0].FullName
  $parts = @((Join-Path $root 'ucrt'))
  foreach ($sub in 'shared', 'um', 'winrt') {
    $p = Join-Path $root $sub
    if (Test-Path $p) { $parts += $p }
  }
  return ($parts -join ';')
}

$msvcInclude = Resolve-MsvcInclude
$sdkInclude  = Resolve-SdkInclude
$includeValue = "$msvcInclude;$sdkInclude"
Write-Host "    INCLUDE repair: MSVC=$msvcInclude"
Write-Host "                    SDK =$sdkInclude"

# ------------------------------------------------------------------ 1. version
Write-Step 'version bump'
$v = & (Join-Path $PSScriptRoot 'New-BuildVersion.ps1') -ProjectRoot $Root -OutputHeader $Header
Set-Content -Path (Join-Path $Root 'expected_version.txt') -Value $v.VersionString -Encoding ASCII
Write-Host "    version = $($v.VersionString)"

# ------------------------------------------------------- 1b. ABI drift check
# RpShim.h and rust/src/ffi.rs are hand-written twins. Adding an rp_* function
# or an RP_* constant on one side only is NOT a compile error: the mismatch
# surfaces at link time at best, and silently reads a wrong value at worst.
# Cheap to check, expensive to miss.
Write-Step 'ABI drift check (RpShim.h vs ffi.rs)'
$abiCheck = Join-Path $PSScriptRoot 'check_abi.py'
$python = Get-Command python -ErrorAction SilentlyContinue
if (!$python) { $python = Get-Command py -ErrorAction SilentlyContinue }
if (!$python) {
  Write-Host '    python not on PATH; skipped' -ForegroundColor Yellow
} else {
  & $python.Source $abiCheck
  if ($LASTEXITCODE -ne 0) {
    throw "ABI drift: RpShim.h and rust/src/ffi.rs disagree (exit $LASTEXITCODE)"
  }
}

# --------------------------------------------------------------- 2. cargo
$rustLib = ''
if ($StubRust) {
  # ---------------------------------------------------------- 2b. C stub lib
  # Temporary: stand in for the Rust library so everything downstream can be
  # validated without a Rust toolchain. cl.exe/lib.exe come from the VS2019
  # install the EDK2 build uses anyway.
  Write-Step 'build C stub static library (-StubRust)'
  $vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2019\Professional\VC\Auxiliary\Build\vcvars64.bat'
  if (!(Test-Path $vcvars)) { throw "vcvars64.bat not found: $vcvars" }
  $objDir = Join-Path $PSScriptRoot '_stub'
  New-Item -ItemType Directory -Force -Path $objDir | Out-Null
  $src = Join-Path $PSScriptRoot '_ruststub.c'
  # /GS- /GR- /Zl: no stack cookies, no RTTI, no default-library record --
  # an EFI-freestanding object that drags in nothing from the CRT.
  $stubCmd = "`"$vcvars`" >nul && cd /d `"$objDir`" && " +
             "cl /nologo /c /O2 /GS- /GR- /Zl `"$src`" /Fo`"stub.obj`" && " +
             "lib /nologo /OUT:`"rp_core.lib`" stub.obj"
  $stubOut = cmd /c "$stubCmd 2>&1"
  $stubOut | Out-Host
  if ($LASTEXITCODE -ne 0) { throw "stub compile/lib failed with exit code $LASTEXITCODE" }

  $rustLib = Join-Path $ModuleDir 'rp_core.lib'
  Copy-Item (Join-Path $objDir 'rp_core.lib') $rustLib -Force
  Write-Host ("    stub lib -> rp_core.lib ({0} bytes)" -f (Get-Item $rustLib).Length)
} elseif (!$SkipRust) {
  Write-Step 'cargo build (x86_64-unknown-uefi, staticlib)'

  # Resolve cargo by ABSOLUTE PATH, preferring the toolchain's own binary over
  # the rustup shim in %USERPROFILE%\.cargo\bin.
  #
  # Why not just `cargo` off PATH: the rustup shim is a proxy exe, and in this
  # workspace it never returns -- the build hangs with no output at all. The
  # toolchain binary at
  #     <rustup home>\toolchains\stable-x86_64-pc-windows-msvc\bin\cargo.exe
  # is the real compiler driver: no proxy hop, and it locates rustc as its own
  # sibling. Verified by hand:
  #     <that path>\cargo.exe --version  ->  cargo 1.98.1 (797e8a9bc 2026-08-05)
  # (rustup itself, `rustup run stable cargo --version`, also works; it is only
  # the shim that is unreliable.)
  $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
  $tcBin = Join-Path $rustupHome 'toolchains\stable-x86_64-pc-windows-msvc\bin'
  $cargo = @(
    (Join-Path $tcBin 'cargo.exe'),
    (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe')
  ) | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (!$cargo) {
    $onPath = Get-Command cargo -ErrorAction SilentlyContinue
    if ($onPath) { $cargo = $onPath.Source }
  }
  if (!$cargo) {
    throw "no cargo found. Install the Rust toolchain (rustup-init.exe), then:`n  rustup target add x86_64-unknown-uefi`nLooked for:`n  $(Join-Path $tcBin 'cargo.exe')`n  $(Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe')`n(To validate everything except the Rust side, re-run with -StubRust.)"
  }
  Write-Host "    cargo = $cargo"

  # Run cargo through cmd.exe with the redirection done BY CMD.
  #
  # Why not call it directly: cargo writes its progress ("   Compiling ...") and
  # its final "    Finished `dev` profile ..." line to STDERR, not stdout. Under
  # $ErrorActionPreference='Stop', PowerShell 5.1 wraps native stderr into a
  # terminating ErrorRecord and the script dies with
  #     NativeCommandError: ... Finished `dev` profile ...
  # even though cargo succeeded. Letting cmd own the 2>&1 keeps native stderr
  # away from PowerShell entirely -- the same trick the EDK2 stage below uses.
  # Paths here are space-free (D:\Work\Code\RustInUEFI\rust and the rustup home),
  # so cmd's quoting rules are not a hazard. The log is kept for inspection.
  New-Item -ItemType Directory -Force -Path $RunLogs | Out-Null
  $cargoLog = Join-Path $RunLogs 'cargo.log'
  # Keep the toolchain bin dir on PATH so any bare-name helper cargo/rustc may
  # spawn resolves inside the same toolchain.
  if ($env:Path -notlike "*$tcBin*") { $env:Path = "$tcBin;$env:Path" }
  $cargoSteps = @()
  if ($Clean) { $cargoSteps += "`"$cargo`" clean" }
  $cargoArgs = @('build')
  if ($Target -eq 'RELEASE') { $cargoArgs += '--release' }
  $cargoSteps += "`"$cargo`" $($cargoArgs -join ' ')"
  $cargoCmd = "cd /d `"$RustDir`" && " + ($cargoSteps -join ' && ') + " > `"$cargoLog`" 2>&1"
  $null = cmd /c $cargoCmd
  $cargoExit = $LASTEXITCODE
  if (Test-Path $cargoLog) { Get-Content $cargoLog | Out-Host }
  if ($cargoExit -ne 0) { throw "cargo build failed with exit code $cargoExit (see $cargoLog)" }

  # ------------------------------------------------------------- 3. normalize
  Write-Step 'normalize static library'
  # cargo names the artifact after the crate (rust/ Cargo.toml: name =
  # "rustupaint"), and rustc picks a prefix/suffix from how the target spec
  # reports its object format. We copy it to a fixed name, rp_core.lib, so the
  # INF has exactly one path to depend on -- and so that name does NOT collide
  # with EDK2's own "<BASE_NAME>.lib" module archive (see RustPaint.inf).
  $profile = if ($Target -eq 'RELEASE') { 'release' } else { 'debug' }
  $candidates = @(
    (Join-Path $RustDir "target\x86_64-unknown-uefi\$profile\rustupaint.lib"),
    (Join-Path $RustDir "target\x86_64-unknown-uefi\$profile\librustupaint.a")
  )
  $src = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (!$src) {
    throw "Rust static library not found. Looked for:`n  $($candidates -join "`n  ")"
  }
  $rustLib = Join-Path $ModuleDir 'rp_core.lib'
  Copy-Item -Path $src -Destination $rustLib -Force
  $size = (Get-Item $rustLib).Length
  Write-Host ("    {0} -> rp_core.lib ({1} bytes)" -f (Split-Path -Leaf $src), $size)
  if ($size -lt 1024) { throw "rp_core.lib is suspiciously small ($size bytes)" }
} else {
  $rustLib = Join-Path $ModuleDir 'rp_core.lib'
  if (!(Test-Path $rustLib)) { throw "-SkipRust given but $rustLib does not exist" }
  Write-Host "    -SkipRust: reusing existing rp_core.lib"
}

# ---------------------------------------------------------------- 4. EDK2
$efi = ''
if (!$SkipEdk2) {
  Write-Step "EDK2 build ($Target, VS2019, X64)"

  # Force the module to recompile.
  #
  # Version.h is regenerated on EVERY build, and EDK2's incremental builder
  # does not track it: the module's objects are considered up to date, nothing
  # is recompiled or relinked, and the .efi keeps reporting the PREVIOUS
  # version. Observed for real -- a build stamped 0.1.0.12 shipped an .efi whose
  # serial log said APP_VERSION=0.1.0.8. The fail-closed version assertion in
  # Test-AppVersion.ps1 caught it, which is exactly its job, but the fix belongs
  # here: delete this module's output so its three C files are rebuilt against
  # the fresh Version.h.
  #
  # Only the module's own directory is removed -- LvglLib/LvglUefiPort stay
  # cached, so this costs a couple of seconds, not a full LVGL rebuild.
  #
  # [System.IO.Directory]::Delete, NOT Remove-Item: this session's PowerShell
  # profile wraps Remove-Item with a fail-closed "move to the recycle bin"
  # hook, and trashing a directory inside an EDK2 build tree aborts the whole
  # script with
  #   [safe-delete][SAFE_DELETE_FAIL_CLOSED] "Some operations were aborted"
  # A direct .NET call is not intercepted. The target is a regenerable EDK2
  # build intermediate (nothing the user authored), and only this module's own
  # output directory is touched.
  $modOut = Join-Path $Edk2 "Build\RustPaintPkg\${Target}_VS2019\X64\RustPaintPkg\Application\RustPaint\RustPaint"
  if (Test-Path $modOut) {
    [System.IO.Directory]::Delete($modOut, $true)
    Write-Host "    cleaned module output (forces rebuild against the new Version.h)"
  }

  Push-Location $Edk2
  try {
    # PYTHONUTF8=1: BaseTools' AutoGen logging thread throws UnicodeEncodeError
    # per line under a GBK console (observed at 100k+ lines); the build still
    # usually completes, but the log becomes useless. Set it for this build
    # only and restore, so the caller's environment is untouched.
    $prevPyUtf8 = $env:PYTHONUTF8
    $env:PYTHONUTF8 = '1'
    try {
      # cmd /c with "2>&1" INSIDE the cmd string: the redirection is done by
      # cmd.exe, so PowerShell never sees native stderr (PS 5.1 wraps native
      # stderr into ErrorRecords that fight $ErrorActionPreference='Stop'),
      # yet MSVC's diagnostics still reach us.
      # ".\edksetup.bat" needs the leading .\ because this machine sets
      # NoDefaultCurrentDirectoryInExePath=1, so a bare name is not resolved.
      #
      # Two fixes are applied AFTER edksetup.bat on purpose, because edksetup
      # is exactly what gets them wrong here:
      #
      #  * INCLUDE -- edksetup runs vcvarsall, which derives the Windows SDK
      #    include dir from the registry; when that query cannot run, INCLUDE
      #    ends up without the UCRT dir and every LVGL TU dies on <stddef.h>.
      #    See the toolchain-env block above.
      #
      #  * PATH is left alone. EDK2's GenFw step is the one tool invoked by
      #    bare name ("GenFw", from the generated Makefile's `GENFW = GenFw`),
      #    and in this shell the child nmake does not see
      #    BaseTools\Bin\Win32 on PATH even though edksetup reports it there.
      #    Prepending it in the cmd chain did not help (and `echo %PATH%`
      #    is expanded at parse time, which makes it a misleading probe).
      #    GenFw.exe itself is present and runs fine when called by absolute
      #    path, so the conversion is simply redone below instead of fighting
      #    PATH. Every other tool (cl/link/lib/nmake) is invoked by absolute
      #    path already, which is why GenFw is the only casualty.
      #  No /v:on: delayed expansion would also mangle edksetup's banners.
      $buildCmd = "set PACKAGES_PATH=$Root;D:\Work\Code&& .\edksetup.bat&& " +
                  "set INCLUDE=$includeValue&& " +
                  "build -p RustPaintPkg/RustPaintPkg.dsc -a X64 -t VS2019 -b $Target"
      $buildOutput = cmd /c "$buildCmd 2>&1"
    } finally {
      if ($null -eq $prevPyUtf8) { Remove-Item Env:PYTHONUTF8 -ErrorAction SilentlyContinue }
      else { $env:PYTHONUTF8 = $prevPyUtf8 }
    }
    # Tee-Object then Out-Host terminates the pipeline: the console still gets
    # the full output and nothing rides the success stream, so a caller that
    # does `$v = & script` keeps a clean capture.
    if ($LogFile -ne '') {
      $buildOutput | Tee-Object -FilePath $LogFile | Out-Host
    } else {
      $buildOutput | Out-Host
    }
    # NOTE: a non-zero exit code is NOT fatal here. The GenFw step at the very
    # end is expected to fail in this shell (see above); everything before it --
    # compile and link (including rp_core.lib) -- has already succeeded and left
    # a valid rustupaint.dll. The next block completes the conversion and only
    # then decides whether we actually failed.
    $edk2Exit = $LASTEXITCODE
  } finally {
    Pop-Location
  }

  $modDir = Join-Path $Edk2 "Build\RustPaintPkg\${Target}_VS2019\X64\RustPaintPkg\Application\RustPaint\RustPaint"
  $efi    = Join-Path $modDir 'OUTPUT\rustupaint.efi'
  $dll    = Join-Path $modDir 'DEBUG\rustupaint.dll'

  if (!(Test-Path $efi)) {
    if (Test-Path $dll) {
      # Complete EDK2's last build step ourselves. GenFw is the ONLY tool the
      # generated Makefile calls by bare name (`GENFW = GenFw`); cl/link/lib/
      # nmake are all absolute paths, so this is the only step the PATH problem
      # above can break. Calling it by absolute path is exact and repeatable.
      Write-Step 'PE -> EFI conversion (GenFw, called by absolute path)'
      $genfw = Join-Path $Edk2 'BaseTools\Bin\Win32\GenFw.exe'
      if (!(Test-Path $genfw)) {
        throw "neither $efi nor a usable GenFw.exe ($genfw) -- EDK2 BaseTools binaries are incomplete"
      }
      & $genfw -e UEFI_APPLICATION -o $efi $dll
      if ($LASTEXITCODE -ne 0 -or !(Test-Path $efi)) {
        throw "GenFw failed (exit $LASTEXITCODE) converting $dll"
      }
      Write-Host "    (workaround) EDK2 exited $edk2Exit at its GenFw step; converted here instead"
    } else {
      throw "EDK2 build failed with exit code $edk2Exit and produced no $dll"
    }
  } elseif ($edk2Exit -ne 0) {
    throw "EDK2 build failed with exit code $edk2Exit"
  }

  Write-Host "    efi = $efi"
}

# ------------------------------------------------------------------ 5. stage
if ($efi -ne '') {
  Write-Step 'stage artifacts'
  $dist = Join-Path $Root 'dist'
  New-Item -ItemType Directory -Force -Path $dist | Out-Null
  Copy-Item $efi (Join-Path $dist 'rustupaint.efi') -Force

  $disk = Join-Path $Root 'qemu_disk'
  New-Item -ItemType Directory -Force -Path $disk | Out-Null
  Copy-Item $efi (Join-Path $disk 'rustupaint.efi') -Force
  # startup.nsh: the Shell runs this automatically from fs0: when it starts.
  # CRLF matters for the EFI shell's line reader.
  Set-Content -Path (Join-Path $disk 'startup.nsh') -Value "rustupaint.efi" -Encoding ASCII
  Copy-Item (Join-Path $Root 'expected_version.txt') (Join-Path $disk 'expected_version.txt') -Force

  # EFI\BOOT\BOOTX64.EFI must be present or BDS reports
  #   "No bootable option or device was found"
  # and never reaches the app. Stage the EDK2 Shell there: the firmware loads
  # the Shell, the Shell runs startup.nsh, startup.nsh runs rustupaint.efi.
  # (Without a firmware that has a built-in Shell on its own filesystem, this
  # staged copy IS the Shell -- do not drop it and hope for a fallback.)
  $bootDir = Join-Path $disk 'EFI\BOOT'
  New-Item -ItemType Directory -Force -Path $bootDir | Out-Null
  $shellCandidates = @(
    (Join-Path $Edk2 'Build\EmulatorX64\DEBUG_VS2019\X64\Shell.efi'),
    'D:\Work\Code\guedit\qemu_disk\EFI\BOOT\BOOTX64.EFI'
  ) | Where-Object {
    # Reject placeholders: D:\Work\Code\GopApp\Shell.efi is a 9-byte stub, and
    # a truncated Shell looks exactly like "the app never started".
    (Test-Path $_) -and ((Get-Item $_).Length -gt 100000)
  }
  $shell = $shellCandidates | Select-Object -First 1
  if (!$shell) {
    throw "no usable Shell.efi found (need >100KB). Looked at:`n  " + (($shellCandidates) -join "`n  ")
  }
  Copy-Item $shell (Join-Path $bootDir 'BOOTX64.EFI') -Force
  Write-Host ("    qemu_disk: rustupaint.efi + startup.nsh + EFI\BOOT\BOOTX64.EFI ({0} bytes)" -f (Get-Item (Join-Path $bootDir 'BOOTX64.EFI')).Length)
}

if ($LogFile -ne '') {
  Add-Content -Path $LogFile -Value ("BUILT: {0}" -f $v.VersionString)
}
Write-Host ""
Write-Host "BUILT: $($v.VersionString)" -ForegroundColor Green
$v
