# rustupaint 与 uefi-rs 的关系，以及为什么没用它

> 本文回答一个具体问题：本项目用 Rust 写 UEFI 应用，为什么一行 `uefi-rs` 都没用。
> 相关背景：[可行性调研.md](可行性调研.md)、[CLAUDE.md](../CLAUDE.md)。

---

## 0. 结论先行：没有依赖关系

本项目**不使用** uefi-rs，也不依赖它生态中的任何 crate。两条可复现的证据：

```bash
# 1. Cargo.toml 里根本没有 [dependencies] 段
sed -n '/^\[package\]/,/^\[profile/p' rust/Cargo.toml

# 2. Rust 侧源码搜不到任何 uefi:: 调用
grep -rn "uefi::\|use uefi\|extern crate uefi" rust/src/     # 无输出
```

`rust/Cargo.toml` 只有 `[package]` / `[lib]` / `[profile.*]`，Rust 侧唯一的 `extern crate` 是 `alloc`。
`rust/src/` 全目录（2540 行，8 个模块）搜不到 `uefi::`。

所以准确的说法不是"我们基于 uefi-rs 做了二次开发"，而是：**我们和它是同一层的两条平行路线**。

---

## 1. uefi-rs 是什么

`rust-osdev/uefi-rs`，给 Rust **应用**用的 UEFI 协议安全封装（下表数据于 2026-09-28 核实）：

| 项 | 值 |
|---|---|
| 主 crate | `uefi`，最新 **0.40.0**（2026-08-27） |
| 配套 | `uefi-raw` 0.16.0（裸绑定）、`uefi-macros` |
| MSRV | **1.91**（不需要 nightly，除非开 `unstable`） |
| 许可 | MIT OR Apache-2.0 |
| 规模 | 约 29K SLoC，约 14 万次下载/月 |
| 常用 feature | `alloc` + `global_allocator`、`logger`、`panic_handler`、`log-debugcon`、`qemu`（panic 走 qemu-exit，CI 可拿退出码）、`fs` |

它提供的正是本项目**刻意绕开**的那一层：GOP 帧缓冲、文件系统、全局分配器、panic handler、logger。
用它的话，cargo 一条命令就能直接产出 `.efi`，不需要 EDK2。

---

## 2. 两条路线的硬指标对比

| 维度 | uefi-rs 路线 | rustupaint（本项目） |
|---|---|---|
| Rust 侧 crate 依赖 | `uefi` 等 | **零** |
| 谁接触 UEFI API | Rust 直接调 boot/runtime services | **C 侧 EDK2**；Rust 完全不接触 |
| 链接产物 | cargo 直接出 `.efi` | cargo 出 `staticlib` → **EDK2 当顶层链接器**出 `.efi` |
| 图形栈 | GOP 裸帧缓冲，自己画 | **LVGL 9.2.2**（C） |
| 需要 EDK2 | 不需要（有 OVMF 即可） | 需要（链接器 + LvglPkg） |
| 内存分配 | `global_allocator` 直接用 UEFI pool | no_std + `alloc`，**UI 事件路径零动态分配** |
| 中文字库 | 自备 | 内置 SimSun 3918 字（随二进制链入） |
| ABI 边界 | 无（Rust 直持协议对象） | `RpShim.h/.c`：58 个函数 + 50 个常量，手写双向同步 |
| 代码规模 | — | Rust 2540 行 + C shim 1928 行 |

---

## 3. 唯一的两个交集

不是"完全无关"，有两点是共用的，但都不属于 uefi-rs：

1. **同一个编译目标** `x86_64-unknown-uefi` —— Rust 官方 Tier-2 target，谁都能用，不属于 uefi-rs 项目。
2. **同一个加载契约** —— UEFI Shell 用 `LoadImage()` / `StartImage()` 加载任意 PE/COFF `EFI_APPLICATION`，
   它根本不知道镜像是 Rust、C 还是汇编写的。

分层摆一下（本项目只踩在 L1 上）：

```
L3   uefi-rs（应用封装） / Patina（Rust 固件框架）      ← 本项目都不依赖
─────────────────────────────────────────────────────────
L2   r-efi / uefi-raw（裸 FFI 绑定）
─────────────────────────────────────────────────────────
L1   x86_64-unknown-uefi target                        ← 交集 ①
     EFI_APPLICATION + PE/COFF                         ← 交集 ②
─────────────────────────────────────────────────────────
L0   UEFI Boot Services 表（共同抽象）
     └─ 本项目由 C 侧独占访问，Rust 侧不许碰
```

Patina（微软 / ODP 的 Rust UEFI 固件）那份官方生态对比表说得很清楚：
uefi-rs 这类应用封装"与底层核心是 C 写的（EDK II）还是 Rust 写的（Patina）都兼容，因为 Boot Services 表本身就是共同抽象"。
本项目正是站在"C 核心 + Rust 逻辑"这个组合上。

---

## 4. 为什么没用它：一条被约束逼出来的决策链

不是不知道有 uefi-rs，是**知道而不用**。决策链只有三步，每一步都由前一步锁死：

1. **图形栈定在 LVGL** → LVGL 是 C 库，它的 UEFI 移植层（`LvglUefiPort`）本来就在 `LvglPkg` 里跑通了；
2. **EDK2 已经是顶层链接器** → 既然 C 侧已承担全部固件交互（GOP、USB 鼠标、串口、内存），
   再让 Rust 侧引入 `uefi` crate 去直接持有协议对象，就会出现**两套并存的 UEFI 访问路径**；
3. **边界一旦分叉，比多一层抽象危险得多** → 同一块显存、同一个 pool allocator 被两套代码各自理解，
   出问题时没有单一责任方。所以宁可让 Rust 侧"不认识固件"，只认识一套扁平 C ABI。

这条纪律在代码里是硬的：`RpShim.h` 里只准出现 `UINT32` / `UINT64` 这类标量，
Rust 侧拿到的是 `u64` 不透明句柄（`Obj`），**从不解释它的内容**。

### 4.1 顺带被否决的方案 E：自动生成 LVGL 绑定

同一个取舍树的另一个分支是 `lvgl` crate（`lv_binding_rust` / `lvgl-sys`）——
用 bindgen 在 `build.rs` 里生成 `lvgl-sys`，再 codegen 出 safe 层，Rust 侧直接持有 LVGL 类型。
立项调研 `可行性调研.md` 第 210 行记录过否决理由：

- 钉在 LVGL **8.3.5**（本项目用 9.2.2）；
- 要 `clang`/`libclang` 和 `DEP_LV_CONFIG_PATH`，Cargo × EDK2 双构建体系耦合；
- 更根本的一条：**绑定一旦生成，Rust 侧就必须认识 `lv_obj_t` / `lv_color_t`**，
  "Rust 不认识 LVGL" 这条纪律从根上不成立。

手写 shim 不是因为不知道有绑定，是**知道而不要**。

---

## 5. 这个选择付出了什么代价

诚实列出来，这些是真实成本，不是免费的：

| 代价 | 现状 |
|---|---|
| 必须维护 EDK2 环境 | VS2019 工具链、DSC/INF、INCLUDE 注入等一整套坑（见 `CLAUDE.md` 第 14 节） |
| **外部无法直接构建** | `LvglPkg` 上游 `MikeWuPing/UEFI_Tools` 是私有仓，别人 clone 后卡在这里 |
| 手写 ABI 要双向同步 | `RpShim.h`（58 函数）与 `rust/src/ffi.rs` 手工同步；已用 `tools/check_abi.py` 挂进构建流程拦截漂移 |
| 拿不到现成协议封装 | uefi-rs 的 `fs`、网络、PCI、设备路径等封装用不了，需要时得自己在 C 侧补 shim |
| 全局分配器不托管 | 用不了 `global_allocator`，分配策略要自己规划（当前事件路径零分配，是有意为之） |

其中"外部无法直接构建"这一条目前是**未解决**的：要把 `LvglPkg` 单独开一个公开仓，
再让 rustpaint 用 submodule 引它，才谈得上真正可复现。

---

## 6. 什么情况下应该改成 uefi-rs

这是个可逆决策，触发条件明确：

- **去掉 LVGL，或换成纯 Rust 图形栈**（`embedded-graphics` + `embedded-graphics-gop` 打底，
  抗锯齿需求上 `tiny-skia`，或押注 `rlvgl`）→ uefi-rs 立刻成为更优解，EDK2 可以整个丢掉；
- **要脱离 EDK2 独立构建** → cargo 直接出 `.efi`，CI 友好度大幅提升；
- **要写文件系统 / 网络 / 磁盘协议密集的工具** → uefi-rs 的 `fs`、`proto::pci`、
  `proto::network` 封装能省掉大量 C 侧 shim 工作。

反过来，只要"LVGL + EDK2 资产复用"这个前提还在，现在的分层就更划算。

### 迁移成本评估（若真要迁）

| 项 | 工作量 |
|---|---|
| LVGL 仍要 C 编译 | 除非连图形栈一起换，否则 C 构建体系去不掉 —— 这是主要障碍 |
| `RpShim` 边界整体重写 | 58 个函数 + 50 个常量的边界要重做成 Rust 直调 |
| 放弃 DSC/INF 体系 | 版本号注入、库依赖管理要另起炉灶 |
| 放弃串口版本断言通道 | 现有 QEMU 验证脚本要改（可换 uefi-rs 的 `qemu` feature + debugcon） |

---

## 7. 一句话给外人

如果有人在 README 之外问起，最省事的回答是：

> rustupaint 用 Rust 写业务逻辑，EDK2 做链接器，LVGL 做渲染。
> Rust 侧零 crate 依赖，连 UEFI 头文件都不 include ——
> 它只通过一层手写的 C ABI（58 个函数）跟 C 侧说话。
> uefi-rs 是另一条"纯 Rust 独立出 .efi"的路线，我们没走，因为图形栈锁在 LVGL 上。

---

## English summary

This project does **not** use `uefi-rs`. Zero crate dependencies: `rust/Cargo.toml` has no
`[dependencies]` section, and `rust/src/` contains no `uefi::` call.

| | uefi-rs route | rustupaint |
|---|---|---|
| UEFI access | Rust calls boot services directly | C/EDK2 only; Rust never touches firmware |
| Output | cargo emits `.efi` | cargo emits `staticlib`, **EDK2 links** the `.efi` |
| Graphics | raw GOP framebuffer | LVGL 9.2.2 (C) |
| EDK2 | not needed | required |
| ABI boundary | none | `RpShim.h/.c`, 58 functions + 50 constants, hand-synced |

Only two things are shared, and neither belongs to uefi-rs: the `x86_64-unknown-uefi` Rust
target, and the PE/COFF `EFI_APPLICATION` contract that the Shell loads via `LoadImage()`.

The decision was forced by one constraint: LVGL is a C library whose UEFI port already works
inside `LvglPkg`. Once EDK2/C owns all firmware interaction, adding the `uefi` crate would
create **two parallel UEFI access paths** with no single owner when something breaks. So Rust
is deliberately kept ignorant of the firmware and talks only through a flat C ABI with opaque
`u64` handles. The bindgen route (`lvgl` crate, option E in the feasibility study) was
rejected for the same reason, plus it is pinned to LVGL 8.3.5.

Accepted costs: EDK2 must be maintained, external users cannot build the repo today
(`LvglPkg` lives in a private upstream), and the hand-written ABI needs the `check_abi.py`
guard against drift. Switch to uefi-rs if LVGL is ever dropped for a pure-Rust graphics stack,
or if protocol-heavy features (fs, network, PCI) start dominating.
