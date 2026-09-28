/** @file
  RpShim —— rustupaint 的 C ABI 边界层（Rust 侧唯一可见的接口）。

  设计约束（三条，改本文件前先读）：
   1. **本头文件不得出现任何 LVGL 类型或 LVGL 枚举。**
      Rust 侧不认识 lv_obj_t / lv_color_t / LV_ALIGN_* / LV_EVENT_*；
      一切跨边界的东西都折成下面的稳定枚举与标量（颜色一律
      0x00RRGGBB + 独立 opa 字节）。允许的 EDK2 类型只有 UINT32/UINT64
      这两个标量（Rust 侧对应 u32/u64，ABI 明确）。这样 Rust 侧永不
      include EDK2 头、也不用 bindgen —— 边界的正确性由 C 编译器在
      这一个文件上保证。
   2. **对象句柄是 UINT64 的指针值**，0 表示无效。Rust 侧把它当不透明
      句柄传递，从不解引用。
   3. **事件走单一 trampoline**：C 侧回调把 LVGL 的 event code 翻译成
      RP_EV_*、把键值翻译成 RP_KEY_*，再调 Rust 注册的函数指针。
      新增事件/按键时**两边同时改**，不要透传 LVGL 原始值。

  Copyright (c) 2026, Mike Wu. All rights reserved.
**/
#ifndef RP_SHIM_H_
#define RP_SHIM_H_

#include <Uefi.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------ */
/* 稳定枚举 —— 与 rust/src/ffi.rs 中的常量必须逐条对齐                  */
/* ------------------------------------------------------------------ */

/// 事件码。C 侧把 LV_EVENT_* 翻译成这些值。
#define RP_EV_PRESSED        1u   ///< 指针按下
#define RP_EV_PRESSING       2u   ///< 指针按住移动（拖拽绘制靠它）
#define RP_EV_RELEASED       3u   ///< 指针抬起
#define RP_EV_CLICKED        4u   ///< 完整点击（按下+抬起在同一对象上）
#define RP_EV_VALUE_CHANGED  5u   ///< 值变化
#define RP_EV_FOCUSED        6u   ///< 获得键盘焦点
#define RP_EV_DEFOCUSED      7u   ///< 失去键盘焦点
#define RP_EV_KEY            8u   ///< 按键（event code = RP_EV_KEY 时 key 字段有效）
#define RP_EV_READY          9u   ///< 对象构建完成（布局尺寸已确定）
#define RP_EV_DELETE        10u   ///< 对象即将销毁

/// 按键。C 侧把 LVGL 键值翻译成这些值。RP_KEY_CHAR 时 key 字段是 ASCII 码。
#define RP_KEY_NONE       0u
#define RP_KEY_TAB        1u
#define RP_KEY_TAB_PREV   2u
#define RP_KEY_UP         3u
#define RP_KEY_DOWN       4u
#define RP_KEY_LEFT       5u
#define RP_KEY_RIGHT      6u
#define RP_KEY_ENTER      7u
#define RP_KEY_ESC        8u
#define RP_KEY_BACKSPACE  9u
#define RP_KEY_DELETE    10u
#define RP_KEY_HOME      11u
#define RP_KEY_END       12u
#define RP_KEY_PAGE_UP   13u
#define RP_KEY_PAGE_DOWN 14u
#define RP_KEY_CHAR      15u   ///< 可打印字符，key = ASCII

/// 对齐（与 LV_ALIGN_* 的九宫格一一对应，但取值由本头文件固定）。
#define RP_ALIGN_TOP_LEFT      0
#define RP_ALIGN_TOP_MID       1
#define RP_ALIGN_TOP_RIGHT     2
#define RP_ALIGN_LEFT_MID      3
#define RP_ALIGN_CENTER        4
#define RP_ALIGN_RIGHT_MID     5
#define RP_ALIGN_BOTTOM_LEFT   6
#define RP_ALIGN_BOTTOM_MID    7
#define RP_ALIGN_BOTTOM_RIGHT  8

/// 字体。C 侧映射到 lv_conf.h 里已编入的字库。
#define RP_FONT_SMALL  0   ///< montserrat 12（拉丁小号）
#define RP_FONT_BASE   1   ///< montserrat 14（默认，拉丁）
#define RP_FONT_LARGE  2   ///< montserrat 16（标题，拉丁）
#define RP_FONT_MONO   3   ///< unscii 16（等宽，坐标/数值显示用）
//
// 中文必须走这两个，不能拿 montserrat 凑：montserrat 只有拉丁字形，
// 中文字符在它下面**什么都不画**（连方框都没有），标签会静默变空白。
// SimSun 字库同时含 ASCII，所以纯中文标签可以直接用它；
// 数字/版本号这类仍建议用拉丁字库，观感更整齐。
#define RP_FONT_CJK       4   ///< simsun 16（中文正文）
#define RP_FONT_CJK_SMALL 5   ///< simsun 14（中文小号，如状态栏）

/// 对象标志。
#define RP_FLAG_CLICKABLE   0x0001u  ///< 可点击（会进 Tab 焦点圈）
#define RP_FLAG_SCROLLABLE  0x0002u  ///< 允许滚动
#define RP_FLAG_HIDDEN      0x0004u  ///< 隐藏

/// 对象状态（用于分状态配色）。
#define RP_STATE_DEFAULT  0u
#define RP_STATE_PRESSED  1u
#define RP_STATE_FOCUSED  2u
#define RP_STATE_DISABLED 3u

#define RP_OPA_COVER 255u
#define RP_OPA_TRANS  0u

/* ------------------------------------------------------------------ */
/* 生命周期                                                            */
/* ------------------------------------------------------------------ */

/// 初始化 LVGL 端口（GOP display + 键盘/鼠标 indev + tick + 光标），
/// 并在需要时拉起内嵌 USB HID 鼠标兜底驱动。
///
/// **返回 EFI_STATUS 原值（UINT64）而不是 int**：EFI_STATUS 是 UINTN，
/// 错误码的 bit63 置位；压成 32 位会把它截成看起来像成功的 0x00000002。
/// 成功为 0。
UINT64
rp_init (
  IN UINT64  ImageHandle
  );

/// 主循环节拍：泵输入事件 + lv_timer_handler()。非阻塞。
void
rp_poll (
  VOID
  );

/// 释放 display/indev/缓冲区（含兜底驱动收尾）。
void
rp_deinit (
  VOID
  );

/// 应用版本串（ASCII，形如 "0.1.0.1"）。由 Version.h 生成，与串口
/// APP_VERSION= 行同源。
const char *
rp_version (
  VOID
  );

/* ------------------------------------------------------------------ */
/* 内存 / 日志 / panic 兜底                                            */
/* ------------------------------------------------------------------ */

/// Rust 全局分配器后端（UEFI pool）。对 0 字节请求返回 NULL。
void *
rp_alloc (
  IN UINT64  Size
  );

void
rp_free (
  IN void  *Ptr
  );

void *
rp_realloc (
  IN void    *Ptr,
  IN UINT64   Size
  );

/// ASCII 日志行，落到串口 DEBUG 通道（同 APP_VERSION 通道）。
void
rp_log (
  IN const char  *Msg
  );

/// 带一个整数的日志行（走 %a + %x，避免 RUST 侧做格式化）。
void
rp_log_hex (
  IN const char  *Msg,
  IN UINT64       Value
  );

/// Rust panic 出口：打一行 ERROR 后永久停住（UEFI 里不能 unwind）。
void
rp_panic (
  IN const char  *Msg
  );

/* ------------------------------------------------------------------ */
/* 显示 / 输入                                                         */
/* ------------------------------------------------------------------ */

void
rp_screen_size (
  OUT int  *W,
  OUT int  *H
  );

/// 当前指针位置（屏幕全局坐标；无指针设备时返回 (0,0) 且 *Valid=0）。
///
/// 位置取自**鼠标 indev**，不是 lv_indev_active ()。后者在键鼠双 indev
/// 的工程里会随机指向键盘 indev，导致坐标恒为 (0,0)。
void
rp_mouse_pos (
  OUT int  *X,
  OUT int  *Y,
  OUT int  *Valid
  );

/// LVGL 毫秒时基，供悬停延迟一类需要真实时间的判定使用。
UINT32
rp_now_ms (
  VOID
  );

/// 指针左键是否按下。
int
rp_mouse_down (
  VOID
  );

/// 当前修饰键（1=Ctrl，2=Shift）。
UINT32
rp_mods (
  VOID
  );

/* ------------------------------------------------------------------ */
/* 事件桥                                                              */
/* ------------------------------------------------------------------ */

/// Rust 侧事件处理函数。@a Obj 是事件源句柄，@a Code 是 RP_EV_*，
/// @a Key 仅在 Code == RP_EV_KEY 时有效（RP_KEY_*），@a User 是注册时
/// 传回的 opaque 值。
typedef void (*rp_event_fn)(UINT64 Obj, UINT32 Code, UINT32 Key, UINT64 User);

/// 给对象挂事件处理函数。同一对象重复调用会覆盖（旧函数不再被调用）。
void
rp_on_event (
  IN UINT64        Obj,
  IN rp_event_fn   Fn,
  IN UINT64        User
  );

/// Rust 定时器回调。
typedef void (*rp_timer_fn)(UINT64 User);

/// 创建周期定时器，返回不透明句柄（0 表示失败）。
UINT64
rp_timer_create (
  IN UINT32       PeriodMs,
  IN rp_timer_fn  Fn,
  IN UINT64       User
  );

void
rp_timer_delete (
  IN UINT64  Timer
  );

/* ------------------------------------------------------------------ */
/* 对象创建                                                            */
/* ------------------------------------------------------------------ */

/// 活动屏幕。
UINT64
rp_screen (
  VOID
  );

/// 通用容器（无默认滚动、无默认内边距影响布局）。
UINT64
rp_obj_create (
  IN UINT64  Parent
  );

/// 文本标签。
UINT64
rp_label_create (
  IN UINT64     Parent,
  IN const char *Text
  );

/// 右端钉在 RightX 上的文本标签（用于标题栏右上角的署名这类场景）。
///
/// 与 rp_label_create + rp_set_pos 的区别：后者只能指定**左端**原点，要做
/// 右对齐就得在 Rust 侧估算字符串宽度——比例字体下必然偏，换字号还会崩。
/// 这里由 LVGL 按固定宽度 + LV_TEXT_ALIGN_RIGHT 排版，右对齐是算出来的，
/// 不是估出来的。
///
/// @param[in] RightX  这一行文字右端所在的 x（相对父容器）
/// @param[in] Y       顶端 y
/// @param[in] BoxW    排版宽度。必须宽于最长的那行文本，否则会被**截断**
///                    （LVGL 不会溢出到框外）。
/// @param[in] Font    RP_FONT_* ；传负值表示沿用默认字体。
UINT64
rp_label_right (
  IN UINT64      Parent,
  IN const char *Text,
  IN int         RightX,
  IN int         Y,
  IN int         BoxW,
  IN int         Font
  );

/// 画布。像素格式固定 RGB888（3 字节/像素，字节序 R,G,B），
/// 缓冲由本层分配；用 rp_canvas_buf / rp_canvas_stride 取几何。
/// 像素格式 RGB888（3 字节/像素，R,G,B）。
UINT64
rp_canvas_create (
  IN  UINT64  Parent,
  IN  int     W,
  IN  int     H
  );

/// 同上，但像素格式是 ARGB8888（4 字节/像素，B,G,R,A）且**缓冲初始全透明**。
///
/// 存在的唯一理由是工具栏图标：图标画在不透明的 RGB888 画布上时，那块矩形
/// 底会盖住按钮自己的按下/焦点底色，在按钮中间留下一块颜色不对的方块。
/// 用带 alpha 的画布，只有笔迹像素可见，任何按钮状态都能透出来。
UINT64
rp_canvas_create_argb (
  IN  UINT64  Parent,
  IN  int     W,
  IN  int     H
  );

/// 该画布每像素字节数（3 = RGB888，4 = ARGB8888）；0 表示句柄无效。
/// Rust 侧据此选择写入宽度，避免"建的是 ARGB 却按 RGB 写"这种整屏错位。
int
rp_canvas_bpp (
  IN  UINT64  Canvas
  );

void
rp_obj_delete (
  IN UINT64  Obj
  );

/// 画布像素缓冲首地址（RGB888）。@return NULL 表示尚未设好缓冲。
void *
rp_canvas_buf (
  IN UINT64  Canvas
  );

/// 画布行跨距（字节）。**必须用它算行偏移，不要用 width*3 猜测。**
int
rp_canvas_stride (
  IN UINT64  Canvas
  );

/// 用纯色填满画布。
void
rp_canvas_fill (
  IN UINT64  Canvas,
  IN UINT32  Rgb
  );

/// 通知 LVGL 画布内容已变（重绘到屏幕）。
void
rp_canvas_refresh (
  IN UINT64  Canvas
  );

/// 通知 LVGL 对象内容已变。
void
rp_obj_invalidate (
  IN UINT64  Obj
  );

/* ------------------------------------------------------------------ */
/* 几何与外观                                                          */
/* ------------------------------------------------------------------ */

void
rp_set_pos (
  IN UINT64  Obj,
  IN int     X,
  IN int     Y
  );

void
rp_set_size (
  IN UINT64  Obj,
  IN int     W,
  IN int     H
  );

void
rp_set_align (
  IN UINT64  Obj,
  IN int     Align,
  IN int     Dx,
  IN int     Dy
  );

/// 把 Obj 对齐到 Base 的指定边（相对定位，用于工具栏/状态栏内部排版）。
void
rp_align_to (
  IN UINT64  Obj,
  IN UINT64  Base,
  IN int     Align,
  IN int     Dx,
  IN int     Dy
  );

void
rp_get_pos (
  IN  UINT64  Obj,
  OUT int    *X,
  OUT int    *Y
  );

void
rp_get_size (
  IN  UINT64  Obj,
  OUT int    *W,
  OUT int    *H
  );

void
rp_set_text (
  IN UINT64     Obj,
  IN const char *Text
  );

void
rp_set_text_color (
  IN UINT64  Obj,
  IN UINT32  Rgb
  );

void
rp_set_font (
  IN UINT64  Obj,
  IN int     Font
  );

/// 背景色 + 不透明度（Opa 为 0..255）。
void
rp_set_bg (
  IN UINT64  Obj,
  IN UINT32  Rgb,
  IN UINT32  Opa
  );

void
rp_set_radius (
  IN UINT64  Obj,
  IN int     Radius
  );

void
rp_set_border (
  IN UINT64  Obj,
  IN int     Width,
  IN UINT32  Rgb,
  IN UINT32  Opa
  );

void
rp_set_pad (
  IN UINT64  Obj,
  IN int     PadAll
  );

/// 内容间距（flex 未启用时为对象内子元素的手工间距辅助；本 app 用
/// 绝对布局，保留给对话框按钮行）。
void
rp_set_pad_row (
  IN UINT64  Obj,
  IN int     Gap
  );

/// Win11 的柔和投影。Width 为 0 时关闭。
void
rp_set_shadow (
  IN UINT64  Obj,
  IN int     Width,
  IN UINT32  Rgb,
  IN UINT32  Opa,
  IN int     Dx,
  IN int     Dy
  );

/// 焦点圈（Tab 焦点点亮用）：outline 宽度/颜色/透明度。
void
rp_set_focus_ring (
  IN UINT64  Obj,
  IN int     Width,
  IN UINT32  Rgb,
  IN UINT32  Opa
  );

/// 分状态配色：State 取 RP_STATE_*，只改背景色。
void
rp_set_bg_state (
  IN UINT64  Obj,
  IN int     State,
  IN UINT32  Rgb
  );

/// 分状态边框色。
void
rp_set_border_state (
  IN UINT64  Obj,
  IN int     State,
  IN int     Width,
  IN UINT32  Rgb
  );

void
rp_add_flag (
  IN UINT64  Obj,
  IN UINT32  Flag
  );

void
rp_remove_flag (
  IN UINT64  Obj,
  IN UINT32  Flag
  );

/* ------------------------------------------------------------------ */
/* 键盘焦点组                                                          */
/* ------------------------------------------------------------------ */

/// 把对象加入默认焦点组（只有入组的对象才可能拿到键盘焦点）。
void
rp_group_add (
  IN UINT64  Obj
  );

void
rp_group_remove (
  IN UINT64  Obj
  );

/// 直接把焦点给某对象。
void
rp_group_focus (
  IN UINT64  Obj
  );

/// 焦点前进/后退（Tab / Shift+Tab 的落点）。返回新焦点对象句柄。
UINT64
rp_group_focus_next (
  VOID
  );

UINT64
rp_group_focus_prev (
  VOID
  );

/// 当前焦点对象句柄（无则 0）。
UINT64
rp_group_focused (
  VOID
  );

/// 清空焦点组并丢掉焦点（对话框起来时用，防止键落到主界面）。
void
rp_group_clear (
  VOID
  );

#ifdef __cplusplus
}
#endif

#endif // RP_SHIM_H_
