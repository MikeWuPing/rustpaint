//! 控件原语：主题令牌 + shim 调用的组合，供 `app.rs` 拼界面。
//!
//! 本层不含任何业务判断（不知道有几种工具、菜单该有哪几项），只回答
//! "一个 Win11 风格的圆角卡片 / 一个可聚焦按钮 / 一行文本长什么样"。
//!
//! 关于悬停：LVGL 9 没有 hover 状态（本轮刻意不做"鼠标移上去变色"，
//! 那需要每拍轮询指针位置自己算命中，收益不抵复杂度）。**可感知的
//! 交互反馈由两条承担**：按下态（LV_STATE_PRESSED）与焦点态
//! （LV_STATE_FOCUSED，即 req.md 要的"焦点点亮 / 失焦 lowlight"）。

// 注意是 `self, *` 而不是 `self, Obj`：`OPA_COVER` / `OPA_TRANS` /
// `FONT_*` / `ALIGN_*` / `STATE_*` / `FLAG_*` 这些稳定枚举常量都定义在
// `ffi` 里（它们与 RpShim.h 的宏一一对应），只引入 `Obj` 会让本文件
// 所有裸名引用全部编译失败。
use crate::ffi::{self, *};
use crate::theme::{self, *};

/// 建一个圆角容器（卡片）。`bg_opa` 用 `OPA_COVER` 得到实底，
/// `OPA_TRANS` 得到纯布局用的透明容器。
pub fn card(parent: Obj, x: i32, y: i32, w: i32, h: i32, bg: u32, bg_opa: u32, radius: i32) -> Obj {
    let o = unsafe { ffi::rp_obj_create(parent) };
    unsafe { ffi::rp_set_pos(o, x, y) };
    unsafe { ffi::rp_set_size(o, w, h) };
    unsafe { ffi::rp_set_radius(o, radius) };
    unsafe { ffi::rp_set_bg(o, bg, bg_opa) };
    o
}

/// 带 1px 内描边的卡片（面板、对话框的常见形态）。
pub fn framed_card(
    parent: Obj,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bg: u32,
    radius: i32,
    border: u32,
) -> Obj {
    let o = card(parent, x, y, w, h, bg, OPA_COVER, radius);
    unsafe { ffi::rp_set_border(o, 1, border, OPA_COVER) };
    o
}

/// 浮层卡片（下拉菜单、对话框）：带描边 + Win11 那种很淡的投影。
pub fn floating_card(parent: Obj, x: i32, y: i32, w: i32, h: i32, radius: i32) -> Obj {
    let o = framed_card(parent, x, y, w, h, BG_CARD, radius, BORDER_STRONG);
    unsafe { ffi::rp_set_shadow(o, 18, theme::SHADOW, theme::SHADOW_OPA, 0, 4) };
    unsafe { ffi::rp_set_pad(o, 0) };
    o
}

/// 建一行文本标签（绝对定位）。
pub fn label_at(parent: Obj, x: i32, y: i32, text: &str, font: i32, color: u32) -> Obj {
    let cs = ffi::cstr(text);
    let o = unsafe { ffi::rp_label_create(parent, cs.as_ptr() as *const core::ffi::c_char) };
    unsafe { ffi::rp_set_pos(o, x, y) };
    unsafe { ffi::rp_set_font(o, font) };
    unsafe { ffi::rp_set_text_color(o, color) };
    o
}

/// 建一行**在父容器内居中**的文本标签。
///
/// 居中交给 LVGL 自己算（`LV_ALIGN_CENTER`），不要在 Rust 侧按字符数估
/// 宽度——比例字体下估出来的中心必然偏，而且换字号就崩。
pub fn label_centered(parent: Obj, text: &str, font: i32, color: u32) -> Obj {
    let cs = ffi::cstr(text);
    let o = unsafe { ffi::rp_label_create(parent, cs.as_ptr() as *const core::ffi::c_char) };
    unsafe { ffi::rp_set_align(o, ffi::ALIGN_CENTER, 0, 0) };
    unsafe { ffi::rp_set_font(o, font) };
    unsafe { ffi::rp_set_text_color(o, color) };
    o
}

/// 把容器改造成"可聚焦按钮"：
///   - 常态底色 `norm`；
///   - 按下时 `press`；
///   - 拿到键盘焦点时 `focus`（req.md 的"焦点点亮"）；
///   - 2px 强调色焦点圈 + 进入 Tab 焦点圈。
///
/// 失焦态就是常态底色 —— 这就是"失焦 lowlight"：不需要额外状态，
/// 只要保证焦点态与常态有**明确的视觉差**即可。
pub fn make_focusable(obj: Obj, norm: u32, press: u32, focus: u32) {
    unsafe { ffi::rp_set_bg(obj, norm, OPA_COVER) };
    unsafe { ffi::rp_set_bg_state(obj, ffi::STATE_PRESSED, press) };
    unsafe { ffi::rp_set_bg_state(obj, ffi::STATE_FOCUSED, focus) };
    unsafe { ffi::rp_set_focus_ring(obj, 2, ACCENT, theme::FOCUS_OPA) };
    unsafe { ffi::rp_add_flag(obj, ffi::FLAG_CLICKABLE) };
    unsafe { ffi::rp_group_add(obj) };
}

/// 在父容器内**右对齐**的一行文本，右端点在 `right_x`。
///
/// 不要试图在 Rust 侧按字符数估宽度再做 `label_at`：比例字体下估必偏，
/// 而且一旦换字号就整体错位。这里的右对齐由 LVGL 按 `box_w` 排版得出。
pub fn label_right(
    parent: Obj,
    right_x: i32,
    y: i32,
    box_w: i32,
    text: &str,
    font: i32,
    color: u32,
) -> Obj {
    let cs = ffi::cstr(text);
    let o = unsafe {
        ffi::rp_label_right(
            parent,
            cs.as_ptr() as *const core::ffi::c_char,
            right_x,
            y,
            box_w,
            font,
        )
    };
    unsafe { ffi::rp_set_text_color(o, color) };
    o
}

/// 可聚焦的文本按钮（按钮本体 + 居中标签），返回 (按钮, 标签)。
///
/// 标签固定用 `FONT_CJK`：本工程所有经由此函数出现的文案都是中文
/// （菜单标题、菜单项、对话框按钮）。中文在拉丁字库上会**静默消失**，
/// 所以这里不留"将来可能传个拉丁字库进来"的口子。
pub fn text_button(
    parent: Obj,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    text: &str,
    norm: u32,
    radius: i32,
) -> (Obj, Obj) {
    let btn = card(parent, x, y, w, h, norm, OPA_COVER, radius);
    make_focusable(btn, norm, BG_PRESSED, ACCENT_SOFT);
    let lb = label_centered(btn, text, ffi::FONT_CJK, TEXT_PRIMARY);
    (btn, lb)
}

/// 分隔线（1px 横线）。
pub fn hline(parent: Obj, x: i32, y: i32, w: i32) -> Obj {
    let o = card(parent, x, y, w, 1, BORDER, OPA_COVER, 0);
    o
}

/// 竖向分隔线。
pub fn vline(parent: Obj, x: i32, y: i32, h: i32) -> Obj {
    card(parent, x, y, 1, h, BORDER, OPA_COVER, 0)
}
