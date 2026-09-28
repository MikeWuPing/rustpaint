## @file
# RustPaintPkg platform description.
#
# Builds rustupaint.efi standalone against the stock edk2 MdePkg and the
# sibling LvglPkg; the edk2 tree itself is not modified. X64 only.
#
# DebugLib is mapped to BaseDebugLibSerialPort so QEMU "-serial file:"
# captures DEBUG() output directly (version assertion channel).
#
# The Rust application logic lives in ../rust and is compiled into
# rp_core.lib by cargo; RustPaint.inf links it via /LIBPATH: +
# /DEFAULTLIB: (see that file for why not the [Binaries] section).
#
# Copyright (c) 2026, Mike Wu. All rights reserved.
#
##

[Defines]
  PLATFORM_NAME                  = RustPaintPkg
  PLATFORM_GUID                  = 3A7F2C49-5D16-4E8B-A0F3-6B9E2D841C57
  PLATFORM_VERSION               = 0.1
  DSC_SPECIFICATION              = 0x00010005
  OUTPUT_DIRECTORY               = Build/RustPaintPkg
  SUPPORTED_ARCHITECTURES        = X64
  BUILD_TARGETS                  = DEBUG|RELEASE|NOOPT
  SKUID_IDENTIFIER               = DEFAULT

[LibraryClasses]
  BaseLib|MdePkg/Library/BaseLib/BaseLib.inf
  BaseMemoryLib|MdePkg/Library/BaseMemoryLib/BaseMemoryLib.inf
  UefiApplicationEntryPoint|MdePkg/Library/UefiApplicationEntryPoint/UefiApplicationEntryPoint.inf
  UefiLib|MdePkg/Library/UefiLib/UefiLib.inf
  UefiBootServicesTableLib|MdePkg/Library/UefiBootServicesTableLib/UefiBootServicesTableLib.inf
  UefiRuntimeServicesTableLib|MdePkg/Library/UefiRuntimeServicesTableLib/UefiRuntimeServicesTableLib.inf
  MemoryAllocationLib|MdePkg/Library/UefiMemoryAllocationLib/UefiMemoryAllocationLib.inf
  DebugLib|MdePkg/Library/BaseDebugLibSerialPort/BaseDebugLibSerialPort.inf
  # Fixed mask instead of the PCD-backed MdePkg instance: BasePcdLibNull
  # returns 0 and would mute the serial log (and its ASSERT recurses).
  DebugPrintErrorLevelLib|RustPaintPkg/Library/FixedDebugPrintErrorLevelLib/FixedDebugPrintErrorLevelLib.inf
  # BaseDebugLibSerialPort consumes SerialPortLib; SerialIoLib is the
  # PCD-free 16550 COM1 (0x3F8) instance, matching QEMU "-serial file:".
  SerialPortLib|PcAtChipsetPkg/Library/SerialIoLib/SerialIoLib.inf
  IoLib|MdePkg/Library/BaseIoLibIntrinsic/BaseIoLibIntrinsic.inf
  RegisterFilterLib|MdePkg/Library/RegisterFilterLibNull/RegisterFilterLibNull.inf
  StackCheckLib|MdePkg/Library/StackCheckLibNull/StackCheckLibNull.inf
  CompilerIntrinsicsLib|MdePkg/Library/CompilerIntrinsicsLib/CompilerIntrinsicsLib.inf
  PcdLib|MdePkg/Library/BasePcdLibNull/BasePcdLibNull.inf
  PrintLib|MdePkg/Library/BasePrintLib/BasePrintLib.inf
  DevicePathLib|MdePkg/Library/UefiDevicePathLib/UefiDevicePathLib.inf
  LvglLib|LvglPkg/Library/LvglLib/LvglLib.inf
  LvglUefiPort|LvglPkg/Library/LvglUefiPort/LvglUefiPort.inf
  # LvglUefiPort's UsbHidMouse.c issues HID class requests (GET/SET_PROTOCOL,
  # CLEAR_FEATURE on a stalled endpoint). The MdePkg instance keeps this from
  # pulling in a new package dependency; its PcdUsbTransferTimeoutValue
  # (FixedAtBuild) is the sanctioned 3s control-transfer timeout.
  # Without this mapping the build dies at:
  #   error 4000: Instance of library class [UefiUsbLib] is not found
  UefiUsbLib|MdePkg/Library/UefiUsbLib/UefiUsbLib.inf

# FixedAtBuild PCDs are baked into every module's AutoGen as
# _gPcd_FixedAtBuild_* constants; the access never routes through PcdLib,
# so value overrides must live in this section. The MdePkg.dec default
# PcdDebugPropertyMask=0 gates off DebugPrintEnabled()/DebugAssertEnabled()
# at every DEBUG()/ASSERT() call site in DebugLib.h, silently killing the
# serial assertion channel.
# 0x03 = DEBUG_PROPERTY_DEBUG_ASSERT_ENABLED | DEBUG_PROPERTY_DEBUG_PRINT_ENABLED.
[PcdsFixedAtBuild]
  gEfiMdePkgTokenSpaceGuid.PcdDebugPropertyMask|0x03
  gEfiMdePkgTokenSpaceGuid.PcdMaximumAsciiStringLength|1000000
  gEfiMdePkgTokenSpaceGuid.PcdMaximumUnicodeStringLength|1000000
  gEfiMdePkgTokenSpaceGuid.PcdDebugClearMemoryValue|0xAF
  gEfiMdePkgTokenSpaceGuid.PcdFixedDebugPrintErrorLevel|0xFFFFFFFF

[Components]
  RustPaintPkg/Application/RustPaint/RustPaint.inf
