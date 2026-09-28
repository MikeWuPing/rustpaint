/** @file
  RpShim 实现：把 LVGL 的对象模型折成 RpShim.h 里那份扁平 C ABI。

  分层职责（不要混）：
    - 本文件只做「翻译」与「资源归属」：LVGL 枚举 → RP_* 枚举、
      lv_color_t ↔ 0x00RRGGBB、canvas 缓冲的分配与记账。
    - 任何业务判断（哪个工具被选中、焦点该去哪、画笔怎么连点）
      都在 Rust 侧，本文件不得出现 rustupaint 的业务概念。

  Copyright (c) 2026, Mike Wu. All rights reserved.
**/

#include "RpShim.h"
#include "Version.h"

#include <Library/LvglLib.h>
#include <Library/LvglUefiPort.h>
#include <Library/DebugLib.h>
#include <Library/BaseLib.h>
#include <Library/BaseMemoryLib.h>
#include <Library/MemoryAllocationLib.h>

/* ------------------------------------------------------------------ */
/* 内部记账                                                            */
/* ------------------------------------------------------------------ */

/// 每对象的事件槽：LVGL 的 event user_data 指向它，Rust 的函数指针存里面。
typedef struct _RP_EVENT_SLOT {
  UINT64                Obj;
  rp_event_fn           Fn;
  UINT64                User;
  struct _RP_EVENT_SLOT *Next;
} RP_EVENT_SLOT;

/// 事件槽链表（退出时统一释放，避免 UEFI pool 泄漏累积）。
STATIC RP_EVENT_SLOT  *mEventSlots = NULL;

/// 定时器槽：lv_timer 的 user_data 指向它。
typedef struct _RP_TIMER_SLOT {
  rp_timer_fn           Fn;
  UINT64                User;
  struct _RP_TIMER_SLOT *Next;
} RP_TIMER_SLOT;

/// 定时器槽链表（同样在退出时统一释放）。
STATIC RP_TIMER_SLOT  *mTimerSlots = NULL;

/// 画布记账：LVGL 只持有我们给的缓冲指针，释放责任在本层。
/// 上限 16 张 = 1 张主画布 + 8 个工具栏图标（见 rust/src/icon.rs）+ 余量。
#define RP_CANVAS_MAX  16
typedef struct {
  UINT64            Obj;
  void             *Raw;      ///< rp_alloc 拿到的原始指针（释放用）
  void             *Aligned;  ///< 对齐后交给 LVGL 的指针（像素访问用）
  UINT32            W;
  UINT32            H;
  UINT32            Stride;
  UINT32            Bpp;      ///< 3 = RGB888，4 = ARGB8888（Rust 侧据此选写入宽度）
} RP_CANVAS_REC;

STATIC RP_CANVAS_REC  mCanvases[RP_CANVAS_MAX];

STATIC BOOLEAN  mInited = FALSE;

//
// 前向声明：RpFindCanvas 的实现在文件后半段，但 rp_canvas_bpp 要用到它。
// 缺了这行就是 C4013（隐式声明返回 int）+ C2040（间接层级不一致），
// 而本模块是按"警告即错误"编的，直接编译失败。
//
STATIC
RP_CANVAS_REC *
RpFindCanvas (
  IN UINT64  Canvas
  );

/* ------------------------------------------------------------------ */
/* 枚举翻译                                                            */
/* ------------------------------------------------------------------ */

/**
  把 LVGL 事件码翻译成 RP_EV_*。返回 0 表示本层不关心，不透传给 Rust。
**/
STATIC
UINT32
RpTranslateEvent (
  IN lv_event_code_t  Code
  )
{
  switch (Code) {
    case LV_EVENT_PRESSED:        return RP_EV_PRESSED;
    case LV_EVENT_PRESSING:       return RP_EV_PRESSING;
    case LV_EVENT_RELEASED:       return RP_EV_RELEASED;
    case LV_EVENT_CLICKED:        return RP_EV_CLICKED;
    case LV_EVENT_VALUE_CHANGED:  return RP_EV_VALUE_CHANGED;
    case LV_EVENT_FOCUSED:        return RP_EV_FOCUSED;
    case LV_EVENT_DEFOCUSED:      return RP_EV_DEFOCUSED;
    case LV_EVENT_KEY:            return RP_EV_KEY;
    case LV_EVENT_READY:          return RP_EV_READY;
    case LV_EVENT_DELETE:         return RP_EV_DELETE;
    default:                      return 0;
  }
}

/**
  把 LVGL 键值翻译成 RP_KEY_*。可打印 ASCII 归到 RP_KEY_CHAR 并把码点
  写进 *CharOut。

  注意：移植层**没有**把 Tab 映射成 LV_KEY_NEXT（它映射成自定义值
  LVGL_KEY_TAB / LVGL_KEY_TAB_PREV，见 LvglUefiPort.h），所以 Tab 的
  焦点导航由应用层显式驱动 rp_group_focus_next/prev，不是 LVGL 自动行为。
**/
STATIC
UINT32
RpTranslateKey (
  IN  UINT32  Key,
  OUT UINT32  *CharOut
  )
{
  *CharOut = 0;

  if (Key == LVGL_KEY_TAB)      return RP_KEY_TAB;
  if (Key == LVGL_KEY_TAB_PREV) return RP_KEY_TAB_PREV;
  if (Key == LVGL_KEY_PAGE_UP)  return RP_KEY_PAGE_UP;
  if (Key == LVGL_KEY_PAGE_DOWN) return RP_KEY_PAGE_DOWN;

  switch (Key) {
    case LV_KEY_UP:        return RP_KEY_UP;
    case LV_KEY_DOWN:      return RP_KEY_DOWN;
    case LV_KEY_LEFT:      return RP_KEY_LEFT;
    case LV_KEY_RIGHT:     return RP_KEY_RIGHT;
    case LV_KEY_ENTER:     return RP_KEY_ENTER;
    case LV_KEY_ESC:       return RP_KEY_ESC;
    case LV_KEY_BACKSPACE: return RP_KEY_BACKSPACE;
    case LV_KEY_DEL:       return RP_KEY_DELETE;
    case LV_KEY_HOME:      return RP_KEY_HOME;
    case LV_KEY_END:       return RP_KEY_END;
    default:               break;
  }

  /* 可打印 ASCII（含空格）：当作字符键上报。 */
  if ((Key >= 0x20) && (Key < 0x7F)) {
    *CharOut = Key;
    return RP_KEY_CHAR;
  }

  return RP_KEY_NONE;
}

/** RP_ALIGN_* → lv_align_t。 */
STATIC
lv_align_t
RpTranslateAlign (
  IN int  Align
  )
{
  switch (Align) {
    case RP_ALIGN_TOP_LEFT:     return LV_ALIGN_TOP_LEFT;
    case RP_ALIGN_TOP_MID:      return LV_ALIGN_TOP_MID;
    case RP_ALIGN_TOP_RIGHT:    return LV_ALIGN_TOP_RIGHT;
    case RP_ALIGN_LEFT_MID:     return LV_ALIGN_LEFT_MID;
    case RP_ALIGN_RIGHT_MID:    return LV_ALIGN_RIGHT_MID;
    case RP_ALIGN_BOTTOM_LEFT:  return LV_ALIGN_BOTTOM_LEFT;
    case RP_ALIGN_BOTTOM_MID:   return LV_ALIGN_BOTTOM_MID;
    case RP_ALIGN_BOTTOM_RIGHT: return LV_ALIGN_BOTTOM_RIGHT;
    case RP_ALIGN_CENTER:
    default:                    return LV_ALIGN_CENTER;
  }
}

/** RP_STATE_* → lv_state_t（用 | LV_PART_MAIN 之前先经此归一）。 */
STATIC
lv_state_t
RpTranslateState (
  IN int  State
  )
{
  switch (State) {
    case RP_STATE_PRESSED:  return LV_STATE_PRESSED;
    case RP_STATE_FOCUSED:  return LV_STATE_FOCUSED;
    case RP_STATE_DISABLED: return LV_STATE_DISABLED;
    case RP_STATE_DEFAULT:
    default:                return LV_STATE_DEFAULT;
  }
}

STATIC
const lv_font_t *
RpTranslateFont (
  IN int  Font
  )
{
  switch (Font) {
    case RP_FONT_SMALL:     return &lv_font_montserrat_12;
    case RP_FONT_LARGE:     return &lv_font_montserrat_16;
    case RP_FONT_MONO:      return &lv_font_unscii_16;
    //
    // 中文用 SimSun：montserrat 只有拉丁字形，中文字符在它下面**什么都画不出来**
    // （没有方框占位），标签会静默变空白。字库在 LvglPkg/Library/LvglLib/Fonts/，
    // 由 lv_conf.h 的 LV_FONT_SIMSUN_16_CJK / 14_CJK 打开。覆盖范围可用
    // tools/font_coverage.py 逐字核对。
    //
    case RP_FONT_CJK:       return &lv_font_simsun_16_cjk;
    case RP_FONT_CJK_SMALL: return &lv_font_simsun_14_cjk;
    case RP_FONT_BASE:
    default:                return &lv_font_montserrat_14;
  }
}

STATIC
UINT32
RpTranslateFlag (
  IN UINT32  Flag
  )
{
  UINT32  Out = 0;

  if ((Flag & RP_FLAG_CLICKABLE) != 0) {
    Out |= LV_OBJ_FLAG_CLICKABLE;
  }
  if ((Flag & RP_FLAG_SCROLLABLE) != 0) {
    Out |= LV_OBJ_FLAG_SCROLLABLE;
  }
  if ((Flag & RP_FLAG_HIDDEN) != 0) {
    Out |= LV_OBJ_FLAG_HIDDEN;
  }
  return Out;
}

/* ------------------------------------------------------------------ */
/* 事件与定时器 trampoline                                             */
/* ------------------------------------------------------------------ */

STATIC
void
RpEventTrampoline (
  IN lv_event_t  *Event
  )
{
  RP_EVENT_SLOT  *Slot;
  UINT32          Code;
  UINT32          Key  = RP_KEY_NONE;
  UINT32          Char = 0;

  Slot = (RP_EVENT_SLOT *)lv_event_get_user_data (Event);
  if ((Slot == NULL) || (Slot->Fn == NULL)) {
    return;
  }

  Code = RpTranslateEvent (lv_event_get_code (Event));
  if (Code == 0) {
    return;
  }

  if (Code == RP_EV_KEY) {
    Key = RpTranslateKey (lv_indev_get_key (lv_indev_active ()), &Char);
    if (Key == RP_KEY_CHAR) {
      Key = Char;
    }
    if (Key == RP_KEY_NONE) {
      return;   /* 不认识的键不透传，避免 Rust 侧收到噪声 */
    }
  }

  Slot->Fn (Slot->Obj, Code, Key, Slot->User);
}

STATIC
void
RpTimerTrampoline (
  IN lv_timer_t  *Timer
  )
{
  RP_TIMER_SLOT  *Slot;

  Slot = (RP_TIMER_SLOT *)lv_timer_get_user_data (Timer);
  if ((Slot != NULL) && (Slot->Fn != NULL)) {
    Slot->Fn (Slot->User);
  }
}

/* ------------------------------------------------------------------ */
/* 生命周期                                                            */
/* ------------------------------------------------------------------ */

UINT64
rp_init (
  IN UINT64  ImageHandle
  )
{
  EFI_STATUS  Status;

  SetMem (mCanvases, sizeof (mCanvases), 0);

  //
  // 鼠标兜底驱动的调用位置是契约的一部分：必须在 LvglPortInit 之前
  // （其内部的 MouseInit 只选一次实例）。固件已有真实指针实例时不能
  // 介入——同一端点上两个消费者会分食报文。
  //
  if (!LvglPortHasRealPointer ()) {
    Status = LvglPortMouseFallbackStart ((EFI_HANDLE)(UINTN)ImageHandle);
    DEBUG ((
      DEBUG_INFO,
      "[RustPaint] mouse fallback start: %r\n",
      Status
      ));
    if (EFI_ERROR (Status) && (Status != EFI_ALREADY_STARTED)) {
      DEBUG ((DEBUG_WARN, "[RustPaint] no pointer device available\n"));
    }
  } else {
    DEBUG ((DEBUG_INFO, "[RustPaint] firmware provides a real pointer device\n"));
  }

  Status = LvglPortInit ();
  if (EFI_ERROR (Status)) {
    DEBUG ((DEBUG_ERROR, "[RustPaint] LvglPortInit failed: %r\n", Status));
    return (UINT64)Status;
  }

  //
  // Win11 风格界面是浅色底，光标必须换成深色箭头——端口层默认那支
  // 是白填充，压在亮底上会整支消失（LvglUefiPort.h 有实测记录）。
  //
  LvglPortSetCursorStyle (LVGL_CURSOR_STYLE_LIGHT_BG);

  mInited = TRUE;
  return 0;
}

void
rp_poll (
  VOID
  )
{
  LvglPortPoll ();
}

void
rp_deinit (
  VOID
  )
{
  RP_EVENT_SLOT  *EventSlot;
  RP_EVENT_SLOT  *NextEvent;
  RP_TIMER_SLOT  *TimerSlot;
  RP_TIMER_SLOT  *NextTimer;
  UINTN           Index;

  if (!mInited) {
    return;
  }

  /* 画布缓冲先还：LVGL 侧不再引用之后才轮到 lv_deinit。 */
  for (Index = 0; Index < RP_CANVAS_MAX; Index++) {
    if (mCanvases[Index].Raw != NULL) {
      rp_free (mCanvases[Index].Raw);
      mCanvases[Index].Raw     = NULL;
      mCanvases[Index].Aligned = NULL;
      mCanvases[Index].Obj     = 0;
    }
  }

  /* 槽表随应用退出一并归还。UEFI pool 不随镜像退出回收——反复
     run/exit 每轮都会累积，必须显式释放（同 guedit 的 ShutdownEditor）。 */
  for (EventSlot = mEventSlots; EventSlot != NULL; EventSlot = NextEvent) {
    NextEvent   = EventSlot->Next;
    FreePool (EventSlot);
  }
  mEventSlots = NULL;

  for (TimerSlot = mTimerSlots; TimerSlot != NULL; TimerSlot = NextTimer) {
    NextTimer   = TimerSlot->Next;
    FreePool (TimerSlot);
  }
  mTimerSlots = NULL;

  LvglPortDeinit ();
  mInited = FALSE;
}

const char *
rp_version (
  VOID
  )
{
  return RUSTPAINT_VERSION_STR;
}

/* ------------------------------------------------------------------ */
/* 内存 / 日志 / panic                                                 */
/* ------------------------------------------------------------------ */

void *
rp_alloc (
  IN UINT64  Size
  )
{
  if (Size == 0) {
    return NULL;
  }
  return AllocatePool ((UINTN)Size);
}

void
rp_free (
  IN void  *Ptr
  )
{
  if (Ptr != NULL) {
    FreePool (Ptr);
  }
}

void *
rp_realloc (
  IN void    *Ptr,
  IN UINT64   Size
  )
{
  if (Size == 0) {
    if (Ptr != NULL) {
      FreePool (Ptr);
    }
    return NULL;
  }
  if (Ptr == NULL) {
    return AllocatePool ((UINTN)Size);
  }
  return ReallocatePool ((UINTN)0, (UINTN)Size, Ptr);
}

void
rp_log (
  IN const char  *Msg
  )
{
  DEBUG ((DEBUG_INFO, "[RustPaint] %a\n", Msg));
}

void
rp_log_hex (
  IN const char  *Msg,
  IN UINT64       Value
  )
{
  DEBUG ((DEBUG_INFO, "[RustPaint] %a0x%lx\n", Msg, Value));
}

void
rp_panic (
  IN const char  *Msg
  )
{
  DEBUG ((DEBUG_ERROR, "[RustPaint] PANIC: %a\n", Msg));
  /* UEFI 里不能 unwind，也不能静默返回——停在原地等看门狗/用户复位，
     比带着半死状态继续跑更容易定位。 */
  CpuDeadLoop ();
}

/* ------------------------------------------------------------------ */
/* 显示 / 输入                                                         */
/* ------------------------------------------------------------------ */

void
rp_screen_size (
  OUT int  *W,
  OUT int  *H
  )
{
  lv_display_t  *Display = lv_display_get_default ();

  if (Display == NULL) {
    if (W != NULL) {
      *W = 0;
    }
    if (H != NULL) {
      *H = 0;
    }
    return;
  }
  if (W != NULL) {
    *W = (int)lv_display_get_horizontal_resolution (Display);
  }
  if (H != NULL) {
    *H = (int)lv_display_get_vertical_resolution (Display);
  }
}

void
rp_mouse_pos (
  OUT int  *X,
  OUT int  *Y,
  OUT int  *Valid
  )
{
  //
  // 取点必须用**鼠标 indev 本身**，不能用 lv_indev_active ()。
  //
  // lv_indev_active () 返回的是"最近一次被读取的那个 indev"（LVGL 内部的
  // indev_act），工程里同时注册了键盘和鼠标两条 indev，所以它在任意一拍
  // 都可能指向键盘 —— 此时下面的类型检查失败，X/Y 停在 0，表现为状态栏
  // 坐标永远是"位置：x -, y -"。这是本项目踩过的真实缺陷：状态栏看起来
  // 像"没接事件"，实际是拿错了 indev。
  //
  lv_indev_t  *Indev;
  lv_point_t   Point;
  VOID        *MouseIndev;

  if (X != NULL) {
    *X = 0;
  }
  if (Y != NULL) {
    *Y = 0;
  }
  if (Valid != NULL) {
    *Valid = 0;
  }

  MouseIndev = LvglPortGetMouseIndev ();
  if (MouseIndev == NULL) {
    return;
  }

  Indev = (lv_indev_t *)MouseIndev;
  if (lv_indev_get_type (Indev) != LV_INDEV_TYPE_POINTER) {
    return;
  }

  lv_indev_get_point (Indev, &Point);
  if (X != NULL) {
    *X = (int)Point.x;
  }
  if (Y != NULL) {
    *Y = (int)Point.y;
  }
  if (Valid != NULL) {
    *Valid = 1;
  }
}

int
rp_mouse_down (
  VOID
  )
{
  // 与 rp_mouse_pos 同理：必须问鼠标 indev 自己，lv_indev_active () 在
  // 键鼠双 indev 工程里不可靠。
  VOID  *MouseIndev = LvglPortGetMouseIndev ();

  if (MouseIndev == NULL) {
    return 0;
  }
  if (lv_indev_get_type ((lv_indev_t *)MouseIndev) != LV_INDEV_TYPE_POINTER) {
    return 0;
  }
  return (lv_indev_get_state ((lv_indev_t *)MouseIndev) == LV_INDEV_STATE_PRESSED) ? 1 : 0;
}

UINT32
rp_now_ms (
  VOID
  )
{
  // LVGL 的毫秒时基。悬停判定要用真实时间而不是"拍数"：主循环的拍间隔
  // 取决于事件泵的 WaitForEvent 何时返回，从亚毫秒到几十毫秒都在变，按
  // 拍计数会让 tooltip 的延迟时长随负载漂移。
  return (UINT32)lv_tick_get ();
}

UINT32
rp_mods (
  VOID
  )
{
  UINT32  Mods = 0;
  UINT32  Raw  = LvglKbdGetModifiers ();

  if ((Raw & LVGL_KBD_MOD_CTRL) != 0) {
    Mods |= 1u;
  }
  if ((Raw & LVGL_KBD_MOD_SHIFT) != 0) {
    Mods |= 2u;
  }
  return Mods;
}

/* ------------------------------------------------------------------ */
/* 事件与定时器注册                                                    */
/* ------------------------------------------------------------------ */

void
rp_on_event (
  IN UINT64        Obj,
  IN rp_event_fn   Fn,
  IN UINT64        User
  )
{
  RP_EVENT_SLOT  *Slot;

  if (Obj == 0) {
    return;
  }

  /* 同对象重复注册 = 覆盖：改已有槽，而不是再挂一个回调。 */
  for (Slot = mEventSlots; Slot != NULL; Slot = Slot->Next) {
    if (Slot->Obj == Obj) {
      Slot->Fn   = Fn;
      Slot->User = User;
      return;
    }
  }

  Slot = AllocatePool (sizeof (RP_EVENT_SLOT));
  if (Slot == NULL) {
    DEBUG ((DEBUG_ERROR, "[RustPaint] rp_on_event: out of pool\n"));
    return;
  }
  Slot->Obj    = Obj;
  Slot->Fn     = Fn;
  Slot->User   = User;
  Slot->Next   = mEventSlots;
  mEventSlots  = Slot;

  lv_obj_add_event_cb ((lv_obj_t *)(UINTN)Obj, RpEventTrampoline, LV_EVENT_ALL, Slot);
}

UINT64
rp_timer_create (
  IN UINT32       PeriodMs,
  IN rp_timer_fn  Fn,
  IN UINT64       User
  )
{
  RP_TIMER_SLOT  *Slot;
  lv_timer_t     *Timer;

  Slot = AllocatePool (sizeof (RP_TIMER_SLOT));
  if (Slot == NULL) {
    return 0;
  }
  Slot->Fn    = Fn;
  Slot->User  = User;
  Slot->Next  = mTimerSlots;
  mTimerSlots = Slot;

  Timer = lv_timer_create (RpTimerTrampoline, PeriodMs, Slot);
  if (Timer == NULL) {
    mTimerSlots = Slot->Next;
    FreePool (Slot);
    return 0;
  }
  return (UINT64)(UINTN)Timer;
}

void
rp_timer_delete (
  IN UINT64  Timer
  )
{
  lv_timer_t  *T;

  if (Timer == 0) {
    return;
  }
  T = (lv_timer_t *)(UINTN)Timer;
  lv_timer_delete (T);
}

/* ------------------------------------------------------------------ */
/* 对象创建                                                            */
/* ------------------------------------------------------------------ */

UINT64
rp_screen (
  VOID
  )
{
  return (UINT64)(UINTN)lv_screen_active ();
}

UINT64
rp_obj_create (
  IN UINT64  Parent
  )
{
  lv_obj_t  *Obj;
  lv_obj_t  *ParentObj;

  ParentObj = (Parent == 0) ? lv_screen_active () : (lv_obj_t *)(UINTN)Parent;
  Obj       = lv_obj_create (ParentObj);
  if (Obj == NULL) {
    return 0;
  }

  //
  // 抹掉主题给的一切默认（背景/边框/圆角/内边距/滚动），把对象变成
  // 一张白纸：外观全部由 Rust 侧显式设定，避免"看着像对了但差 1px
  // 是主题的 padding"这类不可复现的排版偏移。
  //
  lv_obj_remove_style_all (Obj);
  lv_obj_remove_flag (Obj, LV_OBJ_FLAG_SCROLLABLE);
  return (UINT64)(UINTN)Obj;
}

UINT64
rp_label_create (
  IN UINT64     Parent,
  IN const char *Text
  )
{
  lv_obj_t  *Obj;
  lv_obj_t  *ParentObj;

  ParentObj = (Parent == 0) ? lv_screen_active () : (lv_obj_t *)(UINTN)Parent;
  Obj       = lv_label_create (ParentObj);
  if (Obj == NULL) {
    return 0;
  }
  lv_label_set_text (Obj, (Text == NULL) ? "" : Text);
  lv_obj_remove_flag (Obj, LV_OBJ_FLAG_SCROLLABLE);
  return (UINT64)(UINTN)Obj;
}

/**
  一行的右端钉在 RightX 上的标签。

  LVGL 的 label 默认是 LV_SIZE_CONTENT —— 宽度由文本决定，此时再怎么算坐标
  都是"先量后摆"，而文本宽度只能在 LVGL 侧知道。所以这里给它一个固定宽度
  BoxW，让 LVGL 用 LV_TEXT_ALIGN_RIGHT 自己把字符串推到右边：调用方只指定
  右端点，宽度季节性变化也不需要改代码。

  BoxW 取"比预计最宽的字符串还宽一点"即可。给窄了会被截断而不是溢出。
**/
UINT64
rp_label_right (
  IN UINT64      Parent,
  IN const char *Text,
  IN int         RightX,
  IN int         Y,
  IN int         BoxW,
  IN int         Font
  )
{
  lv_obj_t  *Obj;
  lv_obj_t  *ParentObj;

  ParentObj = (Parent == 0) ? lv_screen_active () : (lv_obj_t *)(UINTN)Parent;
  Obj       = lv_label_create (ParentObj);
  if (Obj == NULL) {
    return 0;
  }
  lv_label_set_text (Obj, (Text == NULL) ? "" : Text);
  lv_obj_remove_flag (Obj, LV_OBJ_FLAG_SCROLLABLE);
  // 顺序要紧：先定宽再定对齐，否则 LVGL 仍按 content 宽度排版，
  // TEXT_ALIGN_RIGHT 无从谈起（表现为"右对齐没生效，还是从左排起"）。
  lv_obj_set_size (Obj, BoxW, 24);
  lv_obj_set_style_text_align (Obj, LV_TEXT_ALIGN_RIGHT, 0);
  lv_obj_set_pos (Obj, RightX - BoxW, Y);
  rp_set_font ((UINT64)(UINTN)Obj, Font);
  return (UINT64)(UINTN)Obj;
}

/// 画布创建的共用实现；格式由两个公开入口决定。
///
/// 两种格式的用途是分开的：
///   * RGB888  —— 主绘图区。不透明，Rust 侧按 3 字节/像素写。
///   * ARGB8888 —— 工具栏图标。**必须带 alpha**，否则图标的矩形底会盖住
///     按钮自己的按下/焦点底色，在按钮中间留下一块颜色不对的方块。
///     用透明底之后，图标只有笔迹像素可见，任何按钮状态都能透出来。
STATIC
UINT64
RpCanvasCreate (
  IN  UINT64             Parent,
  IN  int                W,
  IN  int                H,
  IN  lv_color_format_t  Format
  )
{
  lv_obj_t  *Obj;
  lv_obj_t  *ParentObj;
  UINT32     Stride;
  UINT64     Bytes;
  void      *Raw;
  void      *Aligned;
  UINTN      Index;
  UINT32     Bpp;

  if ((W <= 0) || (H <= 0)) {
    return 0;
  }

  for (Index = 0; Index < RP_CANVAS_MAX; Index++) {
    if (mCanvases[Index].Raw == NULL) {
      break;
    }
  }
  if (Index == RP_CANVAS_MAX) {
    DEBUG ((DEBUG_ERROR, "[RustPaint] too many canvases\n"));
    return 0;
  }

  ParentObj = (Parent == 0) ? lv_screen_active () : (lv_obj_t *)(UINTN)Parent;
  Obj       = lv_canvas_create (ParentObj);
  if (Obj == NULL) {
    return 0;
  }
  lv_obj_remove_style_all (Obj);
  lv_obj_remove_flag (Obj, LV_OBJ_FLAG_SCROLLABLE);

  //
  // 行跨距由 LVGL 自己算（不是 w*bpp）：LVGL 的行可能带对齐填充，用 w*bpp
  // 画出来的图会逐行斜移。缓冲我们自己分配并按 LV_DRAW_BUF_ALIGN 对齐，
  // 这样 lv_draw_buf_init 内部的再对齐是空操作，unaligned_data == data ==
  // 我们给的指针 —— Rust 侧拿这一个指针 + 这一个 stride 直接写像素就是对的。
  //
  Stride = lv_draw_buf_width_to_stride ((UINT32)W, Format);
  Bytes  = ((UINT64)Stride * (UINT64)H) + LV_DRAW_BUF_ALIGN;

  Raw = rp_alloc (Bytes);
  if (Raw == NULL) {
    lv_obj_delete (Obj);
    return 0;
  }
  Aligned = ALIGN_POINTER (Raw, LV_DRAW_BUF_ALIGN);

  lv_canvas_set_buffer (Obj, Aligned, W, H, Format);
  if (Format == LV_COLOR_FORMAT_ARGB8888) {
    // 全透明起点：图标只画笔迹像素，其余留给底下的按钮自己显示。
    lv_canvas_fill_bg (Obj, lv_color_hex (0x000000), LV_OPA_TRANSP);
    lv_obj_set_style_bg_opa (Obj, LV_OPA_TRANSP, 0);
  } else {
    lv_canvas_fill_bg (Obj, lv_color_hex (0xFFFFFF), LV_OPA_COVER);
  }

  Bpp = (Format == LV_COLOR_FORMAT_ARGB8888) ? 4 : 3;

  mCanvases[Index].Obj     = (UINT64)(UINTN)Obj;
  mCanvases[Index].Raw     = Raw;
  mCanvases[Index].Aligned = Aligned;
  mCanvases[Index].W       = (UINT32)W;
  mCanvases[Index].H       = (UINT32)H;
  mCanvases[Index].Stride  = Stride;
  mCanvases[Index].Bpp     = Bpp;

  DEBUG ((
    DEBUG_INFO,
    "[RustPaint] canvas %dx%d bpp=%d stride=%d bytes=%ld\n",
    W, H, (INT32)Bpp, (INT32)Stride, Bytes
    ));
  return (UINT64)(UINTN)Obj;
}

UINT64
rp_canvas_create (
  IN  UINT64  Parent,
  IN  int     W,
  IN  int     H
  )
{
  return RpCanvasCreate (Parent, W, H, LV_COLOR_FORMAT_RGB888);
}

UINT64
rp_canvas_create_argb (
  IN  UINT64  Parent,
  IN  int     W,
  IN  int     H
  )
{
  return RpCanvasCreate (Parent, W, H, LV_COLOR_FORMAT_ARGB8888);
}

int
rp_canvas_bpp (
  IN  UINT64  Canvas
  )
{
  RP_CANVAS_REC  *Rec = RpFindCanvas (Canvas);

  return (Rec == NULL) ? 0 : (int)Rec->Bpp;
}

void
rp_obj_delete (
  IN UINT64  Obj
  )
{
  if (Obj != 0) {
    lv_obj_delete ((lv_obj_t *)(UINTN)Obj);
  }
}

STATIC
RP_CANVAS_REC *
RpFindCanvas (
  IN UINT64  Canvas
  )
{
  UINTN  Index;

  for (Index = 0; Index < RP_CANVAS_MAX; Index++) {
    if (mCanvases[Index].Obj == Canvas) {
      return &mCanvases[Index];
    }
  }
  return NULL;
}

void *
rp_canvas_buf (
  IN UINT64  Canvas
  )
{
  RP_CANVAS_REC  *Rec = RpFindCanvas (Canvas);

  return (Rec == NULL) ? NULL : Rec->Aligned;
}

int
rp_canvas_stride (
  IN UINT64  Canvas
  )
{
  RP_CANVAS_REC  *Rec = RpFindCanvas (Canvas);

  return (Rec == NULL) ? 0 : (int)Rec->Stride;
}

void
rp_canvas_fill (
  IN UINT64  Canvas,
  IN UINT32  Rgb
  )
{
  if (Canvas == 0) {
    return;
  }
  lv_canvas_fill_bg ((lv_obj_t *)(UINTN)Canvas, lv_color_hex (Rgb), LV_OPA_COVER);
}

void
rp_canvas_refresh (
  IN UINT64  Canvas
  )
{
  if (Canvas == 0) {
    return;
  }
  lv_obj_invalidate ((lv_obj_t *)(UINTN)Canvas);
}

void
rp_obj_invalidate (
  IN UINT64  Obj
  )
{
  if (Obj != 0) {
    lv_obj_invalidate ((lv_obj_t *)(UINTN)Obj);
  }
}

/* ------------------------------------------------------------------ */
/* 几何与外观                                                          */
/* ------------------------------------------------------------------ */

void
rp_set_pos (
  IN UINT64  Obj,
  IN int     X,
  IN int     Y
  )
{
  if (Obj != 0) {
    lv_obj_set_pos ((lv_obj_t *)(UINTN)Obj, X, Y);
  }
}

void
rp_set_size (
  IN UINT64  Obj,
  IN int     W,
  IN int     H
  )
{
  if (Obj != 0) {
    lv_obj_set_size ((lv_obj_t *)(UINTN)Obj, W, H);
  }
}

void
rp_set_align (
  IN UINT64  Obj,
  IN int     Align,
  IN int     Dx,
  IN int     Dy
  )
{
  if (Obj != 0) {
    lv_obj_align ((lv_obj_t *)(UINTN)Obj, RpTranslateAlign (Align), Dx, Dy);
  }
}

void
rp_align_to (
  IN UINT64  Obj,
  IN UINT64  Base,
  IN int     Align,
  IN int     Dx,
  IN int     Dy
  )
{
  if ((Obj != 0) && (Base != 0)) {
    lv_obj_align_to (
      (lv_obj_t *)(UINTN)Obj,
      (lv_obj_t *)(UINTN)Base,
      RpTranslateAlign (Align),
      Dx,
      Dy
      );
  }
}

void
rp_get_pos (
  IN  UINT64  Obj,
  OUT int    *X,
  OUT int    *Y
  )
{
  if (Obj == 0) {
    return;
  }
  if (X != NULL) {
    *X = (int)lv_obj_get_x ((lv_obj_t *)(UINTN)Obj);
  }
  if (Y != NULL) {
    *Y = (int)lv_obj_get_y ((lv_obj_t *)(UINTN)Obj);
  }
}

void
rp_get_size (
  IN  UINT64  Obj,
  OUT int    *W,
  OUT int    *H
  )
{
  if (Obj == 0) {
    return;
  }
  if (W != NULL) {
    *W = (int)lv_obj_get_width ((lv_obj_t *)(UINTN)Obj);
  }
  if (H != NULL) {
    *H = (int)lv_obj_get_height ((lv_obj_t *)(UINTN)Obj);
  }
}

void
rp_set_text (
  IN UINT64     Obj,
  IN const char *Text
  )
{
  if (Obj != 0) {
    lv_label_set_text ((lv_obj_t *)(UINTN)Obj, (Text == NULL) ? "" : Text);
  }
}

void
rp_set_text_color (
  IN UINT64  Obj,
  IN UINT32  Rgb
  )
{
  if (Obj != 0) {
    lv_obj_set_style_text_color ((lv_obj_t *)(UINTN)Obj, lv_color_hex (Rgb), 0);
  }
}

void
rp_set_font (
  IN UINT64  Obj,
  IN int     Font
  )
{
  if (Obj != 0) {
    lv_obj_set_style_text_font ((lv_obj_t *)(UINTN)Obj, RpTranslateFont (Font), 0);
  }
}

void
rp_set_bg (
  IN UINT64  Obj,
  IN UINT32  Rgb,
  IN UINT32  Opa
  )
{
  lv_obj_t  *O;

  if (Obj == 0) {
    return;
  }
  O = (lv_obj_t *)(UINTN)Obj;
  lv_obj_set_style_bg_color (O, lv_color_hex (Rgb), 0);
  lv_obj_set_style_bg_opa (O, (lv_opa_t)Opa, 0);
}

void
rp_set_radius (
  IN UINT64  Obj,
  IN int     Radius
  )
{
  if (Obj != 0) {
    lv_obj_set_style_radius ((lv_obj_t *)(UINTN)Obj, Radius, 0);
  }
}

void
rp_set_border (
  IN UINT64  Obj,
  IN int     Width,
  IN UINT32  Rgb,
  IN UINT32  Opa
  )
{
  lv_obj_t  *O;

  if (Obj == 0) {
    return;
  }
  O = (lv_obj_t *)(UINTN)Obj;
  lv_obj_set_style_border_width (O, Width, 0);
  lv_obj_set_style_border_color (O, lv_color_hex (Rgb), 0);
  lv_obj_set_style_border_opa (O, (lv_opa_t)Opa, 0);
}

void
rp_set_pad (
  IN UINT64  Obj,
  IN int     PadAll
  )
{
  if (Obj != 0) {
    lv_obj_set_style_pad_all ((lv_obj_t *)(UINTN)Obj, PadAll, 0);
  }
}

void
rp_set_pad_row (
  IN UINT64  Obj,
  IN int     Gap
  )
{
  if (Obj != 0) {
    lv_obj_set_style_pad_row ((lv_obj_t *)(UINTN)Obj, Gap, 0);
  }
}

void
rp_set_shadow (
  IN UINT64  Obj,
  IN int     Width,
  IN UINT32  Rgb,
  IN UINT32  Opa,
  IN int     Dx,
  IN int     Dy
  )
{
  lv_obj_t  *O;

  if (Obj == 0) {
    return;
  }
  O = (lv_obj_t *)(UINTN)Obj;
  lv_obj_set_style_shadow_width (O, Width, 0);
  lv_obj_set_style_shadow_color (O, lv_color_hex (Rgb), 0);
  lv_obj_set_style_shadow_opa (O, (lv_opa_t)Opa, 0);
  lv_obj_set_style_shadow_offset_x (O, Dx, 0);
  lv_obj_set_style_shadow_offset_y (O, Dy, 0);
}

void
rp_set_focus_ring (
  IN UINT64  Obj,
  IN int     Width,
  IN UINT32  Rgb,
  IN UINT32  Opa
  )
{
  lv_obj_t    *O;
  lv_style_selector_t  Sel;

  if (Obj == 0) {
    return;
  }
  O   = (lv_obj_t *)(UINTN)Obj;
  Sel = (lv_style_selector_t)(LV_PART_MAIN | LV_STATE_FOCUSED);

  lv_obj_set_style_outline_width (O, Width, Sel);
  lv_obj_set_style_outline_color (O, lv_color_hex (Rgb), Sel);
  lv_obj_set_style_outline_opa (O, (lv_opa_t)Opa, Sel);
  /* 焦点圈画在对象外沿，不挤压内容。 */
  lv_obj_set_style_outline_pad (O, 2, Sel);
}

void
rp_set_bg_state (
  IN UINT64  Obj,
  IN int     State,
  IN UINT32  Rgb
  )
{
  lv_style_selector_t  Sel;

  if (Obj == 0) {
    return;
  }
  Sel = (lv_style_selector_t)(LV_PART_MAIN | RpTranslateState (State));
  lv_obj_set_style_bg_color ((lv_obj_t *)(UINTN)Obj, lv_color_hex (Rgb), Sel);
}

void
rp_set_border_state (
  IN UINT64  Obj,
  IN int     State,
  IN int     Width,
  IN UINT32  Rgb
  )
{
  lv_obj_t            *O;
  lv_style_selector_t  Sel;

  if (Obj == 0) {
    return;
  }
  O   = (lv_obj_t *)(UINTN)Obj;
  Sel = (lv_style_selector_t)(LV_PART_MAIN | RpTranslateState (State));

  lv_obj_set_style_border_width (O, Width, Sel);
  lv_obj_set_style_border_color (O, lv_color_hex (Rgb), Sel);
  lv_obj_set_style_border_opa (O, LV_OPA_COVER, Sel);
}

void
rp_add_flag (
  IN UINT64  Obj,
  IN UINT32  Flag
  )
{
  if (Obj != 0) {
    lv_obj_add_flag ((lv_obj_t *)(UINTN)Obj, RpTranslateFlag (Flag));
  }
}

void
rp_remove_flag (
  IN UINT64  Obj,
  IN UINT32  Flag
  )
{
  if (Obj != 0) {
    lv_obj_remove_flag ((lv_obj_t *)(UINTN)Obj, RpTranslateFlag (Flag));
  }
}

/* ------------------------------------------------------------------ */
/* 焦点组                                                              */
/* ------------------------------------------------------------------ */

void
rp_group_add (
  IN UINT64  Obj
  )
{
  lv_group_t  *Group = lv_group_get_default ();

  if ((Obj == 0) || (Group == NULL)) {
    return;
  }
  /* CLICK_FOCUSABLE：点击也能把焦点带过去，符合桌面 app 的直觉。 */
  lv_obj_add_flag ((lv_obj_t *)(UINTN)Obj, LV_OBJ_FLAG_CLICK_FOCUSABLE);
  lv_group_add_obj (Group, (lv_obj_t *)(UINTN)Obj);
}

void
rp_group_remove (
  IN UINT64  Obj
  )
{
  lv_group_t  *Group = lv_group_get_default ();

  if ((Obj == 0) || (Group == NULL)) {
    return;
  }
  lv_group_remove_obj ((lv_obj_t *)(UINTN)Obj);
}

void
rp_group_focus (
  IN UINT64  Obj
  )
{
  lv_group_t  *Group = lv_group_get_default ();

  if ((Obj == 0) || (Group == NULL)) {
    return;
  }
  lv_group_focus_obj ((lv_obj_t *)(UINTN)Obj);
}

UINT64
rp_group_focus_next (
  VOID
  )
{
  lv_group_t  *Group = lv_group_get_default ();

  if (Group == NULL) {
    return 0;
  }
  lv_group_focus_next (Group);
  return (UINT64)(UINTN)lv_group_get_focused (Group);
}

UINT64
rp_group_focus_prev (
  VOID
  )
{
  lv_group_t  *Group = lv_group_get_default ();

  if (Group == NULL) {
    return 0;
  }
  lv_group_focus_prev (Group);
  return (UINT64)(UINTN)lv_group_get_focused (Group);
}

UINT64
rp_group_focused (
  VOID
  )
{
  lv_group_t  *Group = lv_group_get_default ();

  if (Group == NULL) {
    return 0;
  }
  return (UINT64)(UINTN)lv_group_get_focused (Group);
}

void
rp_group_clear (
  VOID
  )
{
  lv_group_t  *Group = lv_group_get_default ();

  if (Group == NULL) {
    return;
  }
  //
  // 只调这一句，不要再补 lv_group_focus_obj(NULL)：那个函数开头就是
  // `if (obj == NULL) return;`，拿它清焦点是空操作（本文件实测过）。
  // lv_group_remove_all_objs 自己会先给当前焦点对象发 DEFOCUSED、把
  // obj_focus 置 NULL，再清成员表——正是这里要的语义。
  //
  lv_group_remove_all_objs (Group);
}
