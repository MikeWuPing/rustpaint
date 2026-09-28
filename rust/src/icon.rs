//! 工具栏图标：把 8 个工具画成 26×26 的透明位图。
//!
//! 为什么是"手绘在画布上"而不是图标字体或文字缩写：
//!
//! * **文字缩写（PN/ER/LN…）不成立**：那是占位方案，既不是图标也不自明。
//! * **图标字体不成立**：这份 LVGL 里 montserrat 只带 24 个符号字形，
//!   SimSun 带 74 个，两者都对不上"铅笔/橡皮/油漆桶/吸管"这套工具集，
//!   硬凑只会得到意思不对的图标。
//! * **LVGL 图形对象拼装不成立**：旋转、三角形、斜块用对象拼会很别扭，
//!   而且每个图标要建好几个对象。
//!
//! 于是直接用既有的光栅原语（`canvas.rs`，本来就在给主画布用）画在各自的
//! 小画布上。零新增依赖，形状完全可控，改一个图标就是改一个函数。
//!
//! **画布必须是 ARGB8888**（见 `RpShim.h` 的 `rp_canvas_create_argb`）：
//! 不透明的 RGB 画布会把图标的整块矩形底盖在按钮上，按钮的按下/焦点底色
//! 就透不出来，中间会出现一块颜色不对的方块。带 alpha 之后只有笔迹像素
//! 可见，任何按钮状态都能正常透出。

use crate::canvas::Doc;
use crate::theme::Tool;

/// 图标画布边长。工具按钮是 40×40，图标居中放，四周各留 7px。
pub const ICON_SIZE: i32 = 26;

/// 把 `tool` 的图标画进 `doc`（须为 ICON_SIZE×ICON_SIZE 的 ARGB8888 画布）。
///
/// `ink` 是笔迹颜色：常态用 `TEXT_PRIMARY`，选中/聚焦用 `ACCENT`。
/// 本函数先清空整幅为透明，所以可以反复调用（选中态变化时重绘）。
pub fn paint(tool: Tool, doc: &mut Doc, ink: u32) {
    doc.clear_transparent();
    match tool {
        Tool::Pencil => pencil(doc, ink),
        Tool::Eraser => eraser(doc, ink),
        Tool::Line => line_tool(doc, ink),
        Tool::Rect => rect_tool(doc, ink),
        Tool::Ellipse => ellipse_tool(doc, ink),
        Tool::Fill => fill_tool(doc, ink),
        Tool::Picker => picker_tool(doc, ink),
        Tool::Clear => clear_tool(doc, ink),
    }
}

/// 梯形填充：上沿 `top_a..top_b`（在 `y_top`）线性插值到下沿 `bot_a..bot_b`
/// （在 `y_bot`），逐行画满。斜块形体用它，比用四条线围一圈再填更省事，
/// 也不会在斜边留下锯齿缝。
fn trapezoid(
    doc: &mut Doc,
    y_top: i32,
    top_a: i32,
    top_b: i32,
    y_bot: i32,
    bot_a: i32,
    bot_b: i32,
    c: u32,
) {
    let span = (y_bot - y_top).max(1);
    for y in y_top..=y_bot {
        let t = y - y_top;
        let a = top_a + (bot_a - top_a) * t / span;
        let b = top_b + (bot_b - top_b) * t / span;
        doc.line(a, y, b, y, c, 0);
    }
}

/* ------------------------------------------------------------------ */
/* 八个图标                                                            */
/* ------------------------------------------------------------------ */

/// 铅笔：笔杆 + 逐级收窄的笔尖 + 尾部方块。尾部那块是刻意的——没有它，
/// 这根斜线和"吸管"几乎一样。
///
/// 笔尖用逐渐变小的方笔尖叠出来（而不是再画一条斜线）：26px 的格子里，
/// "越来越细"比"多一条边"更能读出"尖"这个意思。
fn pencil(d: &mut Doc, ink: u32) {
    d.line(19, 7, 10, 16, ink, 1); // 笔杆（3px 宽）
    d.stamp(8, 18, 1, ink, 255); // 收窄
    d.stamp(6, 20, 1, ink, 255);
    d.stamp(4, 22, 0, ink, 255); // 尖
    d.fill_rect(17, 4, 21, 8, ink); // 笔尾
}

/// 橡皮：斜放的实心块 + 下方两道擦痕。
fn eraser(d: &mut Doc, ink: u32) {
    trapezoid(d, 6, 11, 21, 17, 4, 14, ink);
    d.line(5, 21, 10, 21, ink, 0); // 擦掉的碎屑
    d.line(14, 21, 21, 21, ink, 0);
}

/// 直线：一条斜线 + 两端锚点。
/// 线宽用 3px：1px 斜线在这个尺寸下细到几乎看不见（实测如此），
/// 而工具条上的其他图标都是 3px 级的笔画，粗细要一致。
fn line_tool(d: &mut Doc, ink: u32) {
    d.line(7, 19, 19, 7, ink, 1);
    d.stamp(7, 19, 1, ink, 255);
    d.stamp(19, 7, 1, ink, 255);
}

/// 矩形：描边方框，四角画小锚点（与直线同一套视觉语言）。
fn rect_tool(d: &mut Doc, ink: u32) {
    d.rect_outline(4, 7, 21, 19, ink, 1);
}

/// 椭圆：描边椭圆。
fn ellipse_tool(d: &mut Doc, ink: u32) {
    d.ellipse_outline(3, 6, 22, 19, ink, 1);
}

/// 填充（油漆桶）：上宽下窄的桶 + 桶口横边 + 右侧飞出的颜料滴。
/// 颜料滴是关键的辨识特征——没有它就是一块梯形。
fn fill_tool(d: &mut Doc, ink: u32) {
    trapezoid(d, 10, 5, 17, 21, 9, 15, ink);
    d.fill_rect(4, 8, 18, 9, ink); // 桶口
    d.stamp_round(21, 13, 2, ink); // 颜料滴
    d.stamp_round(21, 9, 1, ink);
}

/// 取色（吸管）：右上方胶头方块 + 细管 + 尖头 + 一滴。
fn picker_tool(d: &mut Doc, ink: u32) {
    d.fill_rect(16, 3, 21, 8, ink); // 胶头
    d.line(18, 7, 8, 17, ink, 0); // 细管
    d.line(8, 17, 5, 20, ink, 1); // 尖头
    d.stamp_round(4, 21, 1, ink);
    d.stamp_round(12, 21, 1, ink); // 取到的那一滴
}

/// 清空：废纸篓（桶盖 + 提手 + 桶身）。
/// 刻意不用"×"：那个形状在工具条里更像"关闭窗口"。
fn clear_tool(d: &mut Doc, ink: u32) {
    d.fill_rect(5, 8, 20, 9, ink); // 桶盖
    d.rect_outline(10, 4, 15, 7, ink, 0); // 提手
    d.rect_outline(7, 10, 18, 22, ink, 1); // 桶身
}
