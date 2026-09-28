//! 与 `RpShim.h` 的 C ABI 对接层。
//!
//! 三条纪律（改本文件前先读）：
//!  1. 本文件里每一个常量都必须与 `RpShim.h` 中的同名宏**逐字一致**；
//!     改一边就必须改另一边。两边一起漂移的后果是无编译错误、只有
//!     行为诡异（比如事件码错位一个）。
//!  2. 这是全工程唯一允许出现 `extern "C"` 声明与 `unsafe` 调用的地方
//!     （另加 `lib.rs` 的分配器/panic 出口）。
//!  3. Rust 侧**不认识**任何 LVGL 概念：这里只有不透明句柄 `Obj`、
//!     整数坐标、`0x00RRGGBB` 颜色、以及下面这些稳定枚举。

#![allow(dead_code)]

use core::ffi::{c_char, c_void};

/// 不透明 UI 对象句柄（就是 C 侧的指针值）。0 表示无效。
pub type Obj = u64;

/* ------------------------------------------------------------------ */
/* 稳定枚举 —— 与 RpShim.h 的宏一一对应                                 */
/* ------------------------------------------------------------------ */

// 事件码
pub const EV_PRESSED: u32 = 1;
pub const EV_PRESSING: u32 = 2;
pub const EV_RELEASED: u32 = 3;
pub const EV_CLICKED: u32 = 4;
pub const EV_VALUE_CHANGED: u32 = 5;
pub const EV_FOCUSED: u32 = 6;
pub const EV_DEFOCUSED: u32 = 7;
pub const EV_KEY: u32 = 8;
pub const EV_READY: u32 = 9;
pub const EV_DELETE: u32 = 10;

// 按键
pub const KEY_NONE: u32 = 0;
pub const KEY_TAB: u32 = 1;
pub const KEY_TAB_PREV: u32 = 2;
pub const KEY_UP: u32 = 3;
pub const KEY_DOWN: u32 = 4;
pub const KEY_LEFT: u32 = 5;
pub const KEY_RIGHT: u32 = 6;
pub const KEY_ENTER: u32 = 7;
pub const KEY_ESC: u32 = 8;
pub const KEY_BACKSPACE: u32 = 9;
pub const KEY_DELETE: u32 = 10;
pub const KEY_HOME: u32 = 11;
pub const KEY_END: u32 = 12;
pub const KEY_PAGE_UP: u32 = 13;
pub const KEY_PAGE_DOWN: u32 = 14;
pub const KEY_CHAR: u32 = 15;

// 对齐
pub const ALIGN_TOP_LEFT: i32 = 0;
pub const ALIGN_TOP_MID: i32 = 1;
pub const ALIGN_TOP_RIGHT: i32 = 2;
pub const ALIGN_LEFT_MID: i32 = 3;
pub const ALIGN_CENTER: i32 = 4;
pub const ALIGN_RIGHT_MID: i32 = 5;
pub const ALIGN_BOTTOM_LEFT: i32 = 6;
pub const ALIGN_BOTTOM_MID: i32 = 7;
pub const ALIGN_BOTTOM_RIGHT: i32 = 8;

// 字体
pub const FONT_SMALL: i32 = 0;
pub const FONT_BASE: i32 = 1;
pub const FONT_LARGE: i32 = 2;
pub const FONT_MONO: i32 = 3;
// 中文只能用这两个：montserrat 只有拉丁字形，中文字符在它下面**什么都画
// 不出来**（连方框都没有），标签会静默变空白。SimSun 字库同时也含 ASCII，
// 但数字/版本号仍建议走拉丁字库，观感更整齐。
pub const FONT_CJK: i32 = 4;
pub const FONT_CJK_SMALL: i32 = 5;

// 对象标志
pub const FLAG_CLICKABLE: u32 = 0x0001;
pub const FLAG_SCROLLABLE: u32 = 0x0002;
pub const FLAG_HIDDEN: u32 = 0x0004;

// 分状态配色
pub const STATE_DEFAULT: i32 = 0;
pub const STATE_PRESSED: i32 = 1;
pub const STATE_FOCUSED: i32 = 2;
pub const STATE_DISABLED: i32 = 3;

pub const OPA_COVER: u32 = 255;
pub const OPA_TRANS: u32 = 0;

/* ------------------------------------------------------------------ */
/* extern 声明 —— 与 RpShim.h 的函数签名一一对应                        */
/* ------------------------------------------------------------------ */

extern "C" {
    // 生命周期
    pub fn rp_init(image_handle: u64) -> u64;
    pub fn rp_poll();
    pub fn rp_deinit();
    pub fn rp_version() -> *const c_char;

    // 内存 / 日志 / panic
    pub fn rp_alloc(size: u64) -> *mut c_void;
    pub fn rp_free(ptr: *mut c_void);
    pub fn rp_realloc(ptr: *mut c_void, size: u64) -> *mut c_void;
    pub fn rp_log(msg: *const c_char);
    pub fn rp_log_hex(msg: *const c_char, value: u64);
    // 返回类型是 `!` 而不是 `()`：C 侧实现以 CpuDeadLoop() 结尾，永不返回
    // （见 RpShim.c）。声明成 `!` 让 panic_handler 能直接用它当发散表达式，
    // 不需要在 Rust 侧再补一个假的 `loop {}`。
    pub fn rp_panic(msg: *const c_char) -> !;

    // 显示 / 输入
    pub fn rp_screen_size(w: *mut i32, h: *mut i32);
    pub fn rp_mouse_pos(x: *mut i32, y: *mut i32, valid: *mut i32);
    pub fn rp_mouse_down() -> i32;
    pub fn rp_mods() -> u32;
    /// LVGL 毫秒时基（悬停延迟用真实时间，不按主循环拍数计）。
    pub fn rp_now_ms() -> u32;

    // 事件桥
    pub fn rp_on_event(obj: Obj, f: EventFn, user: u64);
    pub fn rp_timer_create(period_ms: u32, f: TimerFn, user: u64) -> u64;
    pub fn rp_timer_delete(timer: u64);

    // 对象创建
    pub fn rp_screen() -> Obj;
    pub fn rp_obj_create(parent: Obj) -> Obj;
    pub fn rp_label_create(parent: Obj, text: *const c_char) -> Obj;
    /// 右端对齐标签：`rp_label_right` 的右端点由 (right_x, y) 给出，排版宽度
    /// box_w 必须宽于最长文本。详见 RpShim.h 的注释。
    pub fn rp_label_right(
        parent: Obj,
        text: *const c_char,
        right_x: i32,
        y: i32,
        box_w: i32,
        font: i32,
    ) -> Obj;
    pub fn rp_canvas_create(parent: Obj, w: i32, h: i32) -> Obj;
    pub fn rp_obj_delete(obj: Obj);

    // 画布
    pub fn rp_canvas_buf(canvas: Obj) -> *mut c_void;
    pub fn rp_canvas_stride(canvas: Obj) -> i32;
    pub fn rp_canvas_fill(canvas: Obj, rgb: u32);
    pub fn rp_canvas_refresh(canvas: Obj);
    pub fn rp_obj_invalidate(obj: Obj);
    // ARGB8888 画布（缓冲初始全透明）+ 每像素字节数查询。
    // 图标必须用 ARGB：不透明底会盖住按钮自己的按下/焦点底色。
    pub fn rp_canvas_create_argb(parent: Obj, w: i32, h: i32) -> Obj;
    pub fn rp_canvas_bpp(canvas: Obj) -> i32;

    // 几何与外观
    pub fn rp_set_pos(obj: Obj, x: i32, y: i32);
    pub fn rp_set_size(obj: Obj, w: i32, h: i32);
    pub fn rp_set_align(obj: Obj, align: i32, dx: i32, dy: i32);
    pub fn rp_align_to(obj: Obj, base: Obj, align: i32, dx: i32, dy: i32);
    pub fn rp_get_pos(obj: Obj, x: *mut i32, y: *mut i32);
    pub fn rp_get_size(obj: Obj, w: *mut i32, h: *mut i32);
    pub fn rp_set_text(obj: Obj, text: *const c_char);
    pub fn rp_set_text_color(obj: Obj, rgb: u32);
    pub fn rp_set_font(obj: Obj, font: i32);
    pub fn rp_set_bg(obj: Obj, rgb: u32, opa: u32);
    pub fn rp_set_radius(obj: Obj, radius: i32);
    pub fn rp_set_border(obj: Obj, width: i32, rgb: u32, opa: u32);
    pub fn rp_set_pad(obj: Obj, pad_all: i32);
    pub fn rp_set_pad_row(obj: Obj, gap: i32);
    pub fn rp_set_shadow(obj: Obj, width: i32, rgb: u32, opa: u32, dx: i32, dy: i32);
    pub fn rp_set_focus_ring(obj: Obj, width: i32, rgb: u32, opa: u32);
    pub fn rp_set_bg_state(obj: Obj, state: i32, rgb: u32);
    pub fn rp_set_border_state(obj: Obj, state: i32, width: i32, rgb: u32);
    pub fn rp_add_flag(obj: Obj, flag: u32);
    pub fn rp_remove_flag(obj: Obj, flag: u32);

    // 焦点组
    pub fn rp_group_add(obj: Obj);
    pub fn rp_group_remove(obj: Obj);
    pub fn rp_group_focus(obj: Obj);
    pub fn rp_group_focus_next() -> Obj;
    pub fn rp_group_focus_prev() -> Obj;
    pub fn rp_group_focused() -> Obj;
    pub fn rp_group_clear();
}

/// C 侧事件回调签名。Rust 侧只注册下面这一个 trampoline。
pub type EventFn = extern "C" fn(obj: Obj, code: u32, key: u32, user: u64);
/// C 侧定时器回调签名。
pub type TimerFn = extern "C" fn(user: u64);

/* ------------------------------------------------------------------ */
/* 便利包装                                                            */
/* ------------------------------------------------------------------ */

/// 打印一行 ASCII 日志（走串口 DEBUG 通道，与 APP_VERSION 同一条）。
///
/// 注意：本函数接收 `&str`，但 C 侧只接受 NUL 结尾的 ASCII。**非 ASCII
/// 字符会被静默替换成 '?'** —— 界面上可以出现任意字形，但日志通道
/// 是 ASCII 的（真机控制台没有 CJK 字模，见 advmemtest 的教训）。
pub fn log(msg: &str) {
    let mut buf = [0u8; 256];
    let n = msg.len().min(buf.len() - 1);
    for (i, b) in msg.as_bytes()[..n].iter().enumerate() {
        buf[i] = if *b < 0x80 { *b } else { b'?' };
    }
    buf[n] = 0;
    unsafe { rp_log(buf.as_ptr() as *const c_char) }
}

/// 打印「标签 + 十六进制值」。
pub fn log_hex(tag: &str, value: u64) {
    let mut buf = [0u8; 256];
    let n = tag.len().min(buf.len() - 1);
    for (i, b) in tag.as_bytes()[..n].iter().enumerate() {
        buf[i] = if *b < 0x80 { *b } else { b'?' };
    }
    buf[n] = 0;
    unsafe { rp_log_hex(buf.as_ptr() as *const c_char, value) }
}

/// 把 `&str` 折成 NUL 结尾的 ASCII/UTF-8 缓冲，供 `rp_*_create/text` 使用。
///
/// 界面文案是英文（req.md 要求英文菜单），因此这里直接截断到 127 字节
/// 就够；真超出也只影响那一条文案，不会越界。
pub fn cstr(s: &str) -> [u8; 128] {
    let mut buf = [0u8; 128];
    let n = s.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&s.as_bytes()[..n]);
    buf[n] = 0;
    buf
}

/// 取版本串（ASCII）。
pub fn version() -> &'static str {
    unsafe {
        let p = rp_version();
        if p.is_null() {
            return "unknown";
        }
        let mut len = 0usize;
        while *p.add(len) != 0 && len < 64 {
            len += 1;
        }
        let bytes = core::slice::from_raw_parts(p as *const u8, len);
        core::str::from_utf8_unchecked(bytes)
    }
}
