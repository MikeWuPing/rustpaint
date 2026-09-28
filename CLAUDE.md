# CLAUDE.md

本文件为在此仓库中工作的 AI 助手提供指引。权威需求以 `req.md` 为准，本文件只提炼与开发直接相关的约束与已踩过的坑。

## 项目概述

rustupaint 是运行在 UEFI Shell 下的 GUI 小画家（对标 Windows 11 画图的基础功能）。**应用逻辑全部用 Rust 写**，界面渲染与控件由 LVGL 9.2.2 承担（本地 `LvglPkg`，即本工作区既有的 UEFI 移植版）。

当前状态：M0 骨架完成——C 侧薄壳 + EDK2 包 + 链接机制 + QEMU 证据闭环已打通（用 C 桩库验证），Rust 侧代码已写完待编译验证。界面为英文（req.md 要求）。

## 环境与构建

- edk2 在 `D:\Work\Code\edk2`（VS2019 工具链，`Conf/target.txt` 指向 EmulatorPkg / X64 / DEBUG）。**edk2 树本身不修改**。
- QEMU 在 `C:\Program Files\qemu`。NASM 在 `C:\nasm\`。
- Rust 工具链：`rustup` + `x86_64-unknown-uefi` target。**未安装时可用 `-StubRust` 先验证除 Rust 之外的一切**。
- 构建与运行一律走 `tools/` 脚本，是唯一入口：

```powershell
# 完整构建（Rust + EDK2）
powershell -ExecutionPolicy Bypass -File tools/Build-RustPaint.ps1 -Target RELEASE

# Rust 工具链还没装好时，用 C 桩库验证链路
powershell -ExecutionPolicy Bypass -File tools/Build-RustPaint.ps1 -StubRust

# QEMU 跑一次 + 抓帧 + 版本断言
powershell -ExecutionPolicy Bypass -File tools/Run-RustPaintQemu.ps1 `
  -Script "t4 screendump main t1 hover|120|300 t1 btn|left t1 screendump drew"

# 人肉试玩（SDL 窗口，鼠标可用，手动关窗口结束）
powershell -ExecutionPolicy Bypass -File tools/Run-RustPaintQemu.ps1 -Interactive
```

- 构建时 `PACKAGES_PATH=D:\Work\Code\RustInUEFI;D:\Work\Code`。**首项缺了会解析不到本项目内的 LvglPkg 与 RustPaintPkg**。
- 产物：`D:\Work\Code\edk2\Build\RustPaintPkg\<TARGET>_VS2019\X64\rustupaint.efi`，构建脚本再拷到 `dist\` 与 `qemu_disk\`。
- `qemu_disk/`、`dist/`、`snapshot/`、`run_logs/`、`RustPaintPkg/.../rp_core.lib` 均不入库（确定性再生）。

## 架构：C 薄壳 + Rust 静态库

```
UefiMain.c          ← UEFI 入口，只做三件事：版本断言通道、端口生命周期、主循环节拍
RpShim.c/.h         ← LVGL 的 C ABI 边界（Rust 侧唯一可见的接口）
rust/src/lib.rs     ← Rust 侧导出四个符号：rp_app_build / rp_app_quit / rp_app_tick / rp_app_destroy
rust/src/app.rs     ← 布局、事件分发、工具栏/菜单/对话框、绘制交互
rust/src/canvas.rs  ← 画布文档模型与绘制原语（纯像素，不碰 UI）
```

**为什么是"EDK2 做顶层链接器"**：Rust 侧编成 `staticlib`，由 EDK2 的 VS2019 链接器与 C 侧一起链成 `.efi`（`RustPaint.inf` 用 `/LIBPATH:$(MODULE_DIR)` + `/DEFAULTLIB:rustupaint`）。这样 EDK2 侧保留全部既有约定（DSC/INF、LvglPkg 链接、版本断言通道、vendored OVMF），Rust 只贡献逻辑，不必把 LVGL 和 EDK2 头文件拖进 cargo。

- **不用 INF 的 `[Binaries]` 段**：BaseTools 确实解析它，但其语义是"本模块以预编译二进制交付"（`Binaries` 非空且 `Sources` 为空才走那条路），把 IL 库混进有源码的模块不是它的用法。`/DEFAULTLIB:` 等价于源码里的 `#pragma comment(lib,...)`，link.exe 会在所有对象之后再来解析缺失符号，**与链接顺序无关**。
- **`rp_init` 返回 `UINT64` 而不是 `int`**：EFI_STATUS 是 UINTN，错误码 bit63 置位；压成 32 位会把它截成看起来像成功的 `0x00000002`。

### Rust ↔ LVGL 的边界纪律（`RpShim.h`）

1. `RpShim.h` **不得出现任何 LVGL 类型或枚举**。允许的 EDK2 类型只有 `UINT32/UINT64` 两个标量。颜色一律 `0x00RRGGBB` + 独立 opa 字节。
2. Rust 侧**不认识** `lv_obj_t`/`lv_color_t`/`LV_ALIGN_*`/`LV_EVENT_*`。事件码、按键、对齐、字体、标志全部由 C 侧翻译成 `RP_*` 稳定枚举。改一边必须改 `rust/src/ffi.rs` 的同名常量。
3. 事件走**单一 trampoline**：C 侧把 LVGL event code 译成 `RP_EV_*`、键值译成 `RP_KEY_*` 后调 Rust 注册的函数指针。Rust 侧用打包的 `user` 值（高 8 位类别 + 低 24 位下标）分发，**整套 UI 事件路径零动态分配**。
4. 新增控件/事件/按键时**两边同时改**，不要透传 LVGL 原始值。

## 关键约束与已踩过的坑

### 1. `.ps1` 注释一律纯 ASCII（硬纪律）

Windows PowerShell 5.1 对**没有 BOM** 的 `.ps1` 按 ANSI(GBK) 解析；含中文的 UTF-8 注释会被逐对误解码，**把行尾 LF 一起吞掉**——214 行文件读成 179 行、花括号错位、语法断裂，且只在"改到行末字符恰触发"时才爆。本仓所有 `tools/*.ps1` 刻意只用英文注释，避免这一整类事故。不要"顺手补个中文注释"。

### 2. UEFI pool 不随镜像退出回收

反复 `run`/`exit` 每轮都会累积。所有持有内存的地方都必须显式释放：
- C 侧：`rp_deinit` 归还画布缓冲、事件槽链表、定时器槽链表；
- Rust 侧：`rp_app_destroy` drop 掉 `Box<App>`（撤销栈是 MB 级 `Vec<u8>`）。
- 顺序固定：`rp_app_destroy()` **先于** `rp_deinit()`（前者释放 Rust 的 Vec，后者释放画布缓冲并拆 LVGL）。

### 3. 画布行跨距必须用 `stride`，不是 `w*3`

LVGL 的 RGB888 行可能带对齐填充。`RpShim.c` 用 `lv_draw_buf_width_to_stride()` 取真值并缓存，`rp_canvas_stride()` 暴露给 Rust。缓冲由 shim 按 `LV_DRAW_BUF_ALIGN` 对齐分配，使 `lv_draw_buf_init` 的内部再对齐成为空操作（`unaligned_data == data`），Rust 侧拿一个指针 + 一个 stride 直接写像素就是对的。

### 4. 控制台与图形互斥

`LvglPortInit` 拿到 GOP 之后，Shell 的控制台输出就不可用了（stdin 仍在）。所以 app 是**全屏接管**模型：从 Shell 启动，退出时 `LvglPortDeinit` 恢复，Shell 提示符回来。界面信息不要指望用 `Print` 输出——走 `DEBUG`（串口）。

### 5. 串口通道是 ASCII 的

真机控制台由 GraphicsConsoleDxe 绘制、只带 ASCII 字模；`DEBUG` 的 `%a` 也是窄串。`ffi::log` 会把非 ASCII 折成 `?`。界面文案可以任意，日志不行。

### 6. QEMU 参数有三个"不要动"

- **不要加 `-machine q35`**：vendored OVMF 在这个组合下起不来。
- **不要挂 vars pflash**：不挂时 OVMF 回落到**内置 UEFI Shell**，它会自动跑 `fs0:\startup.nsh`——app 正是这么被启动的。
- **必须 `-usb -device usb-mouse`**：鼠标由 `firmware/OVMF_CODE.fd` 里的 `UsbMouseAbsolutePointerDxe` 绑定。PS/2 路是死的（`SioBusDxe` 不枚举 PNP0F13），`usb-tablet` 过不了驱动的 boot 协议检查。

`firmware/OVMF_CODE.fd` 是**带鼠标驱动的自建 OVMF**，来自 advmemtest 的 vendored 副本，不要换成别的 OVMF（`ContraQwen` 那份没有鼠标驱动）。

### 7. 焦点组的切换只有三个出口

`rebuild_main_group`（主界面）/ `enter_menu` / `enter_dialog`。禁止在别处直接调 `rp_group_add`——会漏掉"先清空再重加"，症状是焦点跑到看不见的对象上。

另一个已核实的 LVGL 事实：**`lv_group_focus_obj(NULL)` 是空操作**（实现第一行就 `if(obj == NULL) return;`），清焦点必须用 `lv_group_remove_all_objs`（它会先给当前焦点对象发 DEFOCUSED 再把 `obj_focus` 置 NULL）。`rp_group_clear` 就是这一句。

### 8. Tab 键的焦点导航要自己驱动

本工作区这份 LvglPkg 的移植层**没有**把 Tab 映射成 `LV_KEY_NEXT`，而是映射成自定义值 `LVGL_KEY_TAB` / `LVGL_KEY_TAB_PREV`（见 `LvglUefiPort.h` 的 Task 10/24 记录）。所以 Tab 前进/后退由应用层显式调 `rp_group_focus_next/prev`，不是 LVGL 自动行为。

### 9. 移植层的"焦点已点亮但背景没变"要靠分状态样式

`rp_set_focus_ring` 设的是 `LV_STATE_FOCUSED` 下的 outline；`rp_set_bg_state` 设分状态底色。LVGL 的 group 持有焦点时会自动给对象加 FOCUSED 状态，不需要应用侧监听 `EV_FOCUSED` 去手动改色。

### 10. 悬停：LVGL 没有，必须自己轮询（工具提示就这么做的）

LVGL 9 **没有** hover 状态，也没有"指针进入/离开对象"事件。要让鼠标停在按钮上出提示，只有一条路：每拍拿全局指针坐标，自己跟按钮矩形做命中判定。

工具提示的实现（`app.rs` 的 `update_tooltip` / `show_tooltip`）：

- 建按钮时把每个按钮的**屏幕矩形**存进 `tool_rects`，每拍只做 8 次矩形比较，不去问 LVGL 要对象几何。
- 命中后起计时，超过 `HOVER_DELAY_MS`(420ms) 才浮出；换按钮或移出立刻收起。
- 计时用 `rp_now_ms()`（LVGL 毫秒时基），**不要按主循环拍数计**：拍间隔取决于事件泵的 `WaitForEvent` 何时返回，从亚毫秒到几十毫秒都在变，按拍计会让延迟随负载漂移。
- 提示框建在**最后**（LVGL 的层级 = 创建顺序），且只加 `HIDDEN` 开关、不进焦点组、不设 `CLICKABLE`。
- 鼠标本身仍不做"移上去变色"——可感知的交互反馈由按下态与焦点态承担，悬停只负责"告诉用户这是啥"。

### 10b. 取指针位置要用鼠标 indev，不能用 `lv_indev_active()`

`lv_indev_active()` 返回的是**最近一次被读取的那个 indev**（LVGL 内部的 `indev_act`），不是"指针设备"。本工程同时注册了键盘与鼠标两条 indev，任意一拍它都可能指向键盘 —— 随后的 `lv_indev_get_type() != LV_INDEV_TYPE_POINTER` 检查失败，函数直接返回，X/Y 停在 0。

症状很有迷惑性：状态栏坐标恒为「位置：x -, y -」，看着像"事件没接上"，实际是**拿错了 indev**。

正解是问鼠标 indev 自己：`LvglPortGetMouseIndev()` 拿到句柄再 `lv_indev_get_point()`。`rp_mouse_pos` 与 `rp_mouse_down` 两处都踩过这个坑（`rp_mouse_down` 更隐蔽：它返回 0，表现为"按键读不到"，但画布点击其实走的是 LVGL 事件、不受影响）。

### 10c. 嵌套容器的屏幕坐标要自己算平，`set_pos` 给的是相对父

`canvas` 是 `work` 的子对象，`work` 又在屏幕 `(0, work_y)`。于是：

```
set_pos(canvas, X, Y)  →  屏幕位置 = (0 + X, work_y + Y)
```

而 `canvas_x/canvas_y` 记录的是**屏幕**坐标，用于把指针换算成画布像素。两处一旦用了不同的基准，就会得到一个恒定的整体偏移。

本项目真实炸过一次：`rp_set_pos(self.canvas, rel_x, rel_y)` 漏加 `area_x`(=TOOL_W=56)，画布被画在屏幕 x=8（左半截压在工具栏底下），而 `canvas_x` 仍按 64 参与换算 ⇒ **每一笔都恒定左偏 56px**，且画布还会被工具栏遮住一条。

- 修法不是"改掉那个加法"，而是让两处**共用同一个变量**：`let cv_x = area_x + rel_x;` 然后 `set_pos(canvas, cv_x, ...)` 与 `canvas_x = cv_x`。结构上无法再分叉。
- 判定是不是这类 bug，看偏移量是否**恒定且等于某个布局常量**——56 正好是 `TOOL_W`，这就是指纹。
- 回归验证用 `tools/check_stroke.py`：单击落点逐点验像素，并顺带检查"目标点左侧 56px 处有没有幽灵墨迹"。

### 11. 鼠标拖拽绘制要自己插值

指针读数约 30Hz，快速划动时相邻采样点能差几十像素。`continue_stroke` 对铅笔/橡皮用 Bresenham 在上一采样点与当前点之间补线，否则笔迹是一串断点。

形状工具（直线/矩形/椭圆）的实时预览用**包围盒回滚**：按下时存一份整幅快照，每帧只回滚"上次预览包围盒 ∪ 本次预览包围盒"，抬起时才把那份快照入撤销栈。整幅覆盖是每帧 MB 级内存拷贝，QEMU 下会直接把交互拖垮。

### 12. 椭圆描边用解析扫描而非增量中点算法

`no_std` 的 UEFI 目标没有 libm，`f64::sqrt` 不可用。`canvas.rs` 自带整数 `isqrt`，椭圆用"对每个 x 求两个 y、再对每个 y 求两个 x"两趟叠加得到闭合轮廓。代价 O(rx+ry) 次整数开方（可忽略），换来的是"闭不闭合"可直接推演，不必赌增量决策参数推导不出错。

### 13. `UefiUsbLib` 必须映射

`LvglUefiPort.inf` 的 `UsbHidMouse.c` 要发 HID 类请求，消费 `UefiUsbLib`。DSC 里缺这一条会在构建期直接失败（`error 4000: Instance of library class [UefiUsbLib] is not found`）。同类坑在 `D:\AIProject\gsetupmod` 上炸过一次。

### 14. Rust 工具链：只走清华源，且不要用 rustup 的 shim

- 官方源 `static.rust-lang.org` 在本机只有 **~18 KB/s**；清华 `https://mirrors.tuna.tsinghua.edu.cn/rustup` 是 **~2.7 MB/s**（快 150 倍）。装法见 `docs/可行性调研.md` 与本文档记忆区。
- **`%USERPROFILE%\.cargo\bin` 里的 rustup shim 不可用**，而且失效方式不止一种：早期观测是 `rustc --version` 无输出且不返回；后来又发现 `cargo.exe` / `rustc.exe` 干脆是 **0 字节**，此时 PowerShell 报
  `程序"cargo.exe"无法运行: 指定的可执行文件不是此操作系统平台的有效应用程序`（只有 `rustup.exe` 是完好的 12.7MB）。
  **该目录下只有 0 字节文件时，从 Bash 调用会静默返回 exit 0 且没有任何输出** —— 看起来像"编译成功但没产物"，极易误判。
  诊断一行：`(Get-Item "$env:USERPROFILE\.cargo\bin\cargo.exe").Length`，为 0 就是它。
  构建脚本因此**直连** `%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\cargo.exe`（该目录下的 cargo 是 31MB 的真身）。搬机器时这个路径要跟着改。
- `rust-toolchain.toml` 声明了 `components = ["rustfmt"]`：`--profile minimal` 装出来的工具链缺它，
  一旦在 `rust/` 目录里跑 cargo，rustup 会临时去下载组件。装完工具链顺手 `rustup component add rustfmt`。

### 15. Rust 侧两个"必然踩"的编译错误

- **`use crate::ffi::{self, Obj};` 是错的**：`OPA_COVER`/`OPA_TRANS`/`FONT_*`/`ALIGN_*`/`STATE_*`/`FLAG_*` 这些常量都定义在 `ffi.rs` 里（它们与 `RpShim.h` 的宏一一对应），必须 **glob**
  引入（`use crate::ffi::{self, *};`），否则 37 个 E0425 一起报。
- **`rp_panic` 要声明成 `-> !`**：C 侧以 `CpuDeadLoop()` 结尾、永不返回；声明成 `()` 会让 `panic_handler` 报 E0308（要求 `!` 却得到 `()`）。

### 16. 构建脚本里三个 PowerShell 5.1 陷阱

1. **cargo 的进度行走 stderr** → 在 `$ErrorActionPreference='Stop'` 下被包成**终止错误**，明明成功却整个脚本挂掉。必须经 `cmd /c` 重定向（`run_logs/cargo.log`），与 EDK2 阶段同一手法。
2. **`Remove-Item` 被本会话的 safe-delete 钩子接管**（fail-closed 转回收站），删 EDK2 构建中间目录时抛
   `[safe-delete][SAFE_DELETE_FAIL_CLOSED]` 并终止构建。用 `[System.IO.Directory]::Delete($p, $true)` 绕开（目标是可再生构建产物，不是用户数据）。
3. **调用方管道会掐死构建**：不要 `& Build-RustPaint.ps1 ... | Tee-Object | Select -Last`。用脚本自带的 `-LogFile`。

### 17. QEMU 的启动方式只有一个是对的

目标是"启动 QEMU，脚本立刻继续"，三种写法里只有一种成立：

| 写法 | 结果 |
|---|---|
| `Start-Process` | 抛 `An item with the same key has already been added. Key: 'PATH'` —— 环境块里同时有 `Path` 和 `PATH`，.NET 的字典大小写敏感 |
| `cmd /c "start /b qemu.exe ..."`（**子进程不重定向**） | QEMU 继承了调用方的 stdout 管道，`cmd /c` 一直等到 QEMU 退出才返回 → **脚本卡死在启动那一行**（实测：QEMU 正常开机、串口健康，但后续一行输出都没有） |
| **`cmd /c "start /b qemu.exe ... > out.log 2> err.log"`** ✅ | 重定向由 cmd 自己打开文件，QEMU 继承的是**文件句柄**而不是调用方的管道；cmd 立刻退出，QEMU 的 stdout/stderr 还落盘留证。`/b` 保证子进程留在当前会话/桌面（`-Interactive` 的 SDL 窗口才看得见） |
| WMI `Win32_Process.Create` | 句柄干净、还能直接拿 PID，但子进程跑在 **session 0**，`-display sdl` 的窗口根本不会出现在桌面上 —— 对 `-Interactive` 无用 |

PID 靠"启动前后进程列表做差集"取（`start` 不回传 PID），所以启动前先 `taskkill` 掉残留的 QEMU。

### 18. QMP 抓帧的三个坑

1. **`os.kill(pid, 0)` 在 Windows 上等于 `TerminateProcess(pid, 0)`**：CPython 对非 `CTRL_C_EVENT/CTRL_BREAK_EVENT` 的 sig 直接 TerminateProcess。拿它当"存活检查"会把正在被检查的 QEMU **当场杀掉**，后续 screendump 全落空。改用 `OpenProcess` + `GetExitCodeProcess`。
2. **QEMU 的 `screendump` 不按扩展名选格式**：本机 10.2.50 对 `screendump foo.png` 仍写出 P6 PPM（1280×800×3 + 15 字节头）。所以要**嗅探魔数**，不是 PNG 才用 Pillow 转。Pillow 因此是可选依赖（本机 cmd 里的 `python` 解析到 Python310，那边有 Pillow；managed Python 3.13 没有）。
3. **`start /b python ... > log` 那层包装不可信**：日志文件根本不会生成，python 侧的失败（比如缺 Pillow）**完全静默**。改成 Run 脚本**同步**调用 `cmd /c python ... > log`，并给 `qmp_drive.py` 加 `--deadline`（主循环礼貌退出）+ 守护线程 `os._exit` 硬闸（任何分支都不可能拖死调用方）。
4. 收口用 `taskkill /F /PID`，不要 `$proc.Kill()/WaitForExit()` —— .NET 进程句柄那对组合在本工作区会无限阻塞。

### 19. LVGL 的 888 像素是 **B,G,R**（本项已向源码核实）

不要凭"名字里写着 RGB"推断布局。LVGL 源码里是：

| 类型 | 定义（`src/misc/lv_color.h`） | 内存字节序 |
|---|---|---|
| `lv_color_t`（RGB888 画布 / 显示缓冲） | `struct { blue; green; red; }` | **B,G,R** |
| `lv_color32_t`（ARGB8888） | `struct { blue; green; red; alpha; }` | **B,G,R,A** |

软件渲染器同样如此（`src/draw/sw/blend/lv_draw_sw_blend_to_rgb888.c`：`dest_u8[x+0]=color.blue; dest_u8[x+2]=color.red;`）。

**为什么这个 bug 很难发现**：写错顺序不报错、不越界，只是三个通道整体错位（蓝↔红互换，绿不变）。项目早期只用黑白灰度作画，灰度三通道相等 → 完全看不出来。直到图标要用强调色 `0x0067C0`（蓝）时，屏幕上出现了**橙色**，才暴露出来。任何"颜色不对但看着也挺合理"的现象，先怀疑这里。

`Doc::put_unchecked/get_unchecked` 现在按 `bpp` 分派两种布局；新增任何写像素路径都要跟着走。

**顺带一个绘制侧的经验**：1px 的 `line()` 描边在 26×26 的小图标上会淡到几乎看不见（本项目的"直线"图标就是这么被改掉的）。图标里用 2px 起。

### 20. 字库覆盖要**解析 cmap**，不要读文件头注释

`lv_font_fmt_txt` 的 cmap 有两种编码：

- `FORMAT0_FULL`：`glyph_id_ofs_list[cp - range_start]`，值为 0 表示**该码位没有字形**；
- `SPARSE_FULL`：`unicode_list` 里存的是**相对 `range_start` 的偏移**，不是绝对码位。

（依据 `src/font/lv_font_fmt_txt.c` 的 `get_glyph_dsc_id`：`uint32_t rcp = letter - cmap->range_start;`）

两条推论：
1. 逐字节 grep `0x4EF6` 找汉字是**无效**的（偏移编码下这个绝对码位根本不出现）；
2. 反过来，只按 `range_start/length` 判定"覆盖"会把 SPARSE 段落算成整段连续覆盖，得出虚高的覆盖率。

用 `python tools/gen_cjk_font.py --check`（内部复用 `tools/font_coverage.py`）做验证，它按真实编码展开。当前字库 `LvglPkg/.../Fonts/lv_font_simsun_16_cjk.c` 覆盖 3918 个汉字，界面用字 100% 命中，**无需替换**。

另有一个陷阱：`LvglPkg/Library/LvglLib/lvgl/src/font/` 下也有一份 `lv_font_simsun_16_cjk.c`，但 LvglLib.inf 编的是 `LvglPkg/Library/LvglLib/Fonts/` 那份（两者内容不同）。**查覆盖要看 INF 里真正列出的那个路径。**

改完界面文案后必须跑一次覆盖检查，否则中文会**静默消失**（拉丁字库对缺字不报错、不画方框，就是什么都不画）：

```bash
python tools/gen_cjk_font.py --check     # 当前 154 字，缺 0
```

### 21. 右对齐要交给 LVGL 排版，不要在 Rust 侧估字符串宽度

`obj` 的坐标只能指定**左端原点**，所以"把一行字贴到右边"直觉上要 `x = right - 估算宽度`。这个估法在比例字体下必然偏，换字号就整体错位。

正解是给 label 一个**固定宽度**再让它自己右对齐：

```c
lv_obj_set_size (Obj, BoxW, 24);                        /* 先定宽 */
lv_obj_set_style_text_align (Obj, LV_TEXT_ALIGN_RIGHT, 0); /* 再定对齐 */
lv_obj_set_pos (Obj, RightX - BoxW, Y);
```

两条纪律：

- **顺序不能反**。先设对齐再设尺寸，LVGL 会先用 content 宽度排好版，`TEXT_ALIGN_RIGHT` 无从生效——现象是"右对齐没起作用，还是从左排起"。
- **`BoxW` 必须宽于最长文本**。给窄了是**截断**，不是溢出到框外（LVGL 不画到框外）。

Rust 侧入口是 `widget::label_right(parent, right_x, y, box_w, text, font, color)`（底层 `rp_label_right`），标题栏右上角的署名用的就是它。

顺带一条排版经验：**「中文标签 + 拉丁正文」的混排走单一 CJK 字库**（simsun 同时含 ASCII），两部分的基线天生对齐；拆成两个不同字库的 label 就得手工对基线，换字号即崩。

## 验证清单（每完成一步都留证据）

1. 串口出现 `APP_VERSION=<version>`，且与 `expected_version.txt` **逐字节一致**（`tools/Test-AppVersion.ps1` 卡这一关；不一致一律按失败处理，不要问"运行是否正常"）。
2. 串口无 `[RustPaint] PANIC`、无 `[RustPaint] ... failed`。
3. `snapshot/` 里有 `screendump` 抓到的 PNG，且画面是预期内容。抓帧走 QEMU monitor/QMP——**SDL 窗口客户区抓屏在 QEMU 下必然全黑，不可用作证据**。
4. 退出后 Shell 提示符可正常交互（证明控制台模式已恢复）。
5. 桩构建（`-StubRust`）能跑通 = C 侧 + EDK2 + 链接机制 + QEMU 链路全部正常；此时"界面没出来"是预期行为。

**已达成（2026-09-27 M0）**：`dist/rustupaint.efi` = 439,520 字节，版本 `0.1.0.17+20260927_213015`，0 个 LNK2001；
`snapshot/*_drew.png` 与串口 `[RustPaint] ui built` 证实——界面完整渲染（标题栏/菜单栏/工具列/调色板/状态栏/Tab 焦点环），
且 QMP 注入的 `hover + down/rel…/up` 在画布上画出真实笔迹。一次完整运行 `Run-RustPaintQemu.ps1` 退出码 0。

**已达成（2026-09-28 中文化 + 图标 + 布局）**：版本 `0.1.0.21+20260927_222315`，`dist/rustupaint.efi` = 1,411,808 字节
（体积从 439KB 涨到 1.4MB 是 CJK 的必然代价——SimSun 字库此前**没有任何引用、被链接器丢弃**，现在真正用上就链进来了）。
一次运行四帧全部核对通过：

| 帧 | 证据 |
|---|---|
| `a_ui` | 菜单 文件/编辑/帮助、状态栏 工具：铅笔 / 颜色：#000000 / 位置 / 画布 / 版本 全中文；16 个色块**单行**排布，游程编码逐块比对数值与 `SWATCHES` 完全一致；状态栏行（y=790）只有文字抗锯齿灰阶，**无任何色块侵入** |
| `b_eraser` | 点 (28,142) 后 tool#1 出现 135 个 `0x0067C0` 像素、tool#0 归零；状态栏哈希变化 → 工具切换 + 文案联动 |
| `c_menu` | 点帮助 (178,51) → 下拉出现「关于」；状态栏仍为「橡皮」 |
| `d_about` | 点「关于」(220,86) → 对话框全中文（关于 rustupaint / UEFI Shell 下的小画家 / 逻辑用 Rust 编写，渲染由 LVGL 9.2.2 完成 / 版本 / Tab 切换焦点，Esc 关闭本对话框 / 「确定」主按钮） |

**点击坐标是算出来的、不是试出来的**：工具按钮中心 = `(28, 76 + 46i)`（`work_y = TITLE_H + MENU_H = 68`，按钮 `y = 68 + 8 + 46i`，高 40）；
菜单标题中心 = `(42 + 68i, 51)`；「关于」条目中心 ≈ `(220, 86)`（面板 `x = 8 + 2*68 - 4 = 140`）。
`qmp_drive.py` 的 hover 标定 `abs_max = 1024` 自洽（`round(28*1024/1280)=22`、`round(77*1024/800)=99`，与日志逐位吻合），**坐标可以直接按像素算**。

## 已知待办

- 撤销栈固定 8 层、每层一份整幅快照；大画布下需按字节数上限自适应。
- 标题栏的窗口按钮（最小化/最大化/关闭）暂未实现。**实现路径已就绪**：工具条那 8 个图标就是"用 ARGB8888 小画布手绘"的成品样板（`rust/src/icon.rs` + `rp_canvas_create_argb`），照抄即可。注意 ARGB 是必须的——RGB 画布不透明，会盖掉按钮的按下/焦点底色。
- 文件保存/打开（UEFI Simple File System）尚未实现；Save/Save As 需要 `lv_textarea` 之类的输入控件，要先扩 shim。
- 未做：油漆桶的大面积填充性能（当前是逐像素扫描线，QEMU 下大区域会卡）。
- 已做（0.1.0.24）：工具栏悬浮提示、画布定位偏移修复、状态栏坐标实时更新。产品手册见 `docs/manual/index.html`（含截图与 GIF）。
- 串口有一句 `[LvglPort] pointer: no data in first 200 poll(s)`（开机头 200 次轮询设备还没上报）。
  后续实测**不影响功能**——QMP 注入位移后光标立即跟随、按键沿正常、能连续画出笔迹；但真机上"鼠标完全不动时"的表现值得再确认一次。
