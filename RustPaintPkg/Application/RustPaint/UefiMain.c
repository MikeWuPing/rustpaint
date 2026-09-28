/** @file
  rustupaint 的 UEFI 入口。

  本文件刻意保持很薄——它只负责三件 C 侧才能做的事：
    1. 串口版本断言通道（APP_VERSION=，供 QEMU 无人值守比对）；
    2. 端口生命周期（rp_init / rp_deinit 的对称调用）；
    3. 主循环节拍（rp_poll + rp_app_tick 交替）。

  界面构建、事件处理、绘图模型**全部**在 Rust（rust/src/lib.rs）。
  本文件不得创建任何 UI 对象、不得 include LVGL 头；反过来 Rust 侧
  不得调用 LvglPort* 系列，也不知道 EFI_HANDLE 是什么（它收到 UINT64）。

  Copyright (c) 2026, Mike Wu. All rights reserved.
**/

#include <Uefi.h>
#include <Library/UefiLib.h>
#include <Library/DebugLib.h>
#include <Library/BaseLib.h>

#include "RpShim.h"
#include "Version.h"

//
// rust/src/lib.rs 导出的应用侧接口（#[no_mangle] extern "C"）。
// 名字与签名必须与 rust/src/ffi.rs 的声明逐字一致。
//
extern int   rp_app_build   (UINT64 ImageHandle);
extern int   rp_app_quit    (void);
extern void  rp_app_tick    (void);
extern void  rp_app_destroy (void);

EFI_STATUS
EFIAPI
UefiMain (
  IN EFI_HANDLE        ImageHandle,
  IN EFI_SYSTEM_TABLE  *SystemTable
  )
{
  EFI_STATUS  Status;
  int         Rc;

  //
  // EDK II PrintLib: %s 是 CHAR16（即使在 ASCII 调试通道上），窄串必须
  // 用 %a。这行是全项目版本断言链路的起点（Test-AppVersion.ps1 比对
  // 它与 expected_version.txt）。
  //
  DEBUG ((DEBUG_INFO, "APP_VERSION=%a\n", RUSTPAINT_VERSION_STR));
  DEBUG ((DEBUG_INFO, "[RustPaint] build=%a\n", RUSTPAINT_BUILD_STR));

  Status = (EFI_STATUS)rp_init ((UINT64)(UINTN)ImageHandle);
  if (EFI_ERROR (Status)) {
    DEBUG ((DEBUG_ERROR, "[RustPaint] rp_init failed: %r\n", Status));
    return Status;
  }

  Rc = rp_app_build ((UINT64)(UINTN)ImageHandle);
  if (Rc != 0) {
    DEBUG ((DEBUG_ERROR, "[RustPaint] rp_app_build failed: %d\n", Rc));
    rp_app_destroy ();
    rp_deinit ();
    return EFI_DEVICE_ERROR;
  }

  //
  // 主循环。rp_poll 内含移植层 1ms 事件泵节拍（WaitForEvent），
  // 不要再补 Stall——纯轮询会饿死固件的输入队列（LvglUefiPort.h 契约）。
  //
  while (!rp_app_quit ()) {
    rp_poll ();
    rp_app_tick ();
  }

  rp_app_destroy ();
  rp_deinit ();

  DEBUG ((DEBUG_INFO, "[RustPaint] exit clean\n"));
  return EFI_SUCCESS;
}
