/** @file
  _ruststub.c -- TEMPORARY stand-in for the Rust static library.

  为什么存在：Rust 工具链就位之前，先把「C 侧 + EDK2 包 + 链接机制 +
  QEMU 全链路」验证掉。本文件提供 rust/src/lib.rs 会导出的那四个符号。

  两条刻意约束：
   1. **不 include 任何头文件**（包括 EDK2 的）。这样 cl.exe 不需要任何
      include 路径即可编出目标文件，桩的构建不引入额外环境依赖。
   2. 需要日志时调用 RpShim.c 已经提供的 rp_log / rp_log_hex，符号在链
      入模块时自然解析 —— 桩只提供 Rust 侧该提供的部分。

  真 Rust 库一到位就停止使用本文件：桩跑出来"界面没出现"是预期行为，
  不代表链路有问题；反过来，桩能跑起来就证明除 Rust 之外都是好的。

  Copyright (c) 2026, Mike Wu. All rights reserved.
**/

extern void rp_log (const char *msg);
extern void rp_log_hex (const char *msg, unsigned long long value);

int
rp_app_build (
  unsigned long long  ImageHandle
  )
{
  rp_log_hex ("(stub) rp_app_build handle=", ImageHandle);
  rp_log ("(stub) this build has no UI - the Rust library is not linked yet");
  return 0;
}

int
rp_app_quit (
  void
  )
{
  return 0;
}

void
rp_app_tick (
  void
  )
{
}

void
rp_app_destroy (
  void
  )
{
  rp_log ("(stub) rp_app_destroy");
}
