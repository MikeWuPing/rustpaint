//! 应用主体：布局、事件分发、工具与绘制交互、菜单与模态对话框。
//!
//! 设计要点（改本文件前先读）：
//!
//! 1. **事件不使用闭包 / trait 对象**：每个可交互对象在注册时携带一个
//!    打包过的 `user` 值（高 8 位是类别、低 24 位是下标），由单一
//!    `trampoline` 按类别分发。整套 UI 事件路径上**零动态分配**，也
//!    不必让 `Box<dyn Fn>` 和 no_std 分配器纠缠。
//!
//! 2. **菜单面板与对话框在构造时一次建好，之后只切 HIDDEN**。每次开合
//!    都重建/删除对象会让 C 侧事件槽链表随使用次数增长，而且让"哪个
//!    对象还存在"变得难以推理。常驻对象一次性消掉这个不确定性。
//!
//! 3. **焦点组成员的切换只有三个出口**：`rebuild_main_group`（主界面）、
//!    `enter_menu`、`enter_dialog`。禁止在别处直接调 `rp_group_add`，
//!    否则会漏掉"先清空再重加"这一步，症状是焦点跑到看不见的对象上。
//!
//! 4. **画布坐标 = 屏幕坐标 − 画布绝对原点**。画布是 `work` 的子对象，
//!    但绝对原点由本文件布局时算定并缓存在 `canvas_x/canvas_y`；不去问
//!    LVGL（问回来的是相对坐标，绕一圈只会多一处出错的地方）。
//!
//! 5. **状态栏只在值真的变化时写 LVGL**。`lv_label_set_text` 会触发重新
//!    排版与重绘；主循环是每拍都跑的，无条件写等于每拍白送一次重绘。

use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;

use crate::canvas::{Doc, Undo};
// 同 widget.rs：必须 glob 引入 `ffi`，否则 OPA_* / FONT_* / ALIGN_* /
// STATE_* / FLAG_* 这些定义在 ffi 里的稳定枚举常量在本文件全都不可见。
use crate::ffi::{self, *};
// 工具栏图标：8 个工具各自画在一张 ARGB8888 小画布上（见 icon.rs）
use crate::icon::{self, ICON_SIZE};
use crate::theme::{self, Tool, *};
use crate::widget;

/* ------------------------------------------------------------------ */
/* 事件 user 值的编码                                                  */
/* ------------------------------------------------------------------ */

const UC_MENU_ROOT: u64 = 1;
const UC_MENU_ROW: u64 = 2;
const UC_TOOL: u64 = 3;
const UC_SWATCH: u64 = 4;
const UC_CANVAS: u64 = 5;
const UC_DLG: u64 = 6;

#[inline]
fn enc(class: u64, index: u32) -> u64 {
    (class << 56) | (index as u64)
}

#[inline]
fn dec(user: u64) -> (u64, u32) {
    (user >> 56, (user & 0xFF_FFFF) as u32)
}

/* ------------------------------------------------------------------ */
/* 菜单定义                                                            */
/* ------------------------------------------------------------------ */

/// 菜单标题与条目。
///
/// 中文文案只走 `FONT_CJK`（见 `widget.rs` 的字体选择）；`FONT_HINT` 那类
/// 拉丁字库上中文会**静默消失**。改这里的文案后跑一次
/// `python tools/gen_cjk_font.py --check` 复核字库覆盖。
const MENU_NAMES: [&str; 3] = ["文件", "编辑", "帮助"];
const MENU_ROWS: [&[&str]; 3] = [&["新建", "退出"], &["撤销", "清空"], &["关于"]];
const MENU_ITEM_W: i32 = 68;
const MENU_ROW_H: i32 = 30;
const MENU_PANEL_W: i32 = 160;
const MENU_MAX_ROWS: usize = 3;

/// 署名。标题栏右上角与「关于」对话框共用同一份字面量，改一处即可。
///
/// 混排中文+拉丁，所以渲染时走 `FONT_CJK_SMALL`（simsun 同时含 ASCII，
/// 两种文字的风格才是一套的）。拆成两个 label 会让两部分基线对不齐。
const AUTHOR_LINE: &str = "作者：Mike Wu";
/// 联系方式，只显示在「关于」对话框里。
const AUTHOR_MAIL: &str = "mikewuping@163.com";
const SWATCH_COUNT: usize = 16;
const ERASER_R: i32 = 5;

/// 指针在同一个工具按钮上停留多久才弹提示（毫秒）。
///
/// 用真实时间而不是主循环拍数：拍间隔取决于事件泵的 WaitForEvent 何时
/// 返回，负载不同能差一个数量级，按拍计数会让延迟随机器状态漂移。
const HOVER_DELAY_MS: u32 = 420;
const TOOLTIP_W: i32 = 168;
const TOOLTIP_H: i32 = 44;
/// 提示框相对按钮的横向偏移：贴在工具栏右边一点，不压住按钮本身。
const TOOLTIP_DX: i32 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    None,
    New,
    Exit,
    Undo,
    Clear,
    About,
}

fn menu_cmd(menu: usize, row: usize) -> Cmd {
    match (menu, row) {
        (0, 0) => Cmd::New,
        (0, 1) => Cmd::Exit,
        (1, 0) => Cmd::Undo,
        (1, 1) => Cmd::Clear,
        (2, 0) => Cmd::About,
        _ => Cmd::None,
    }
}

/* ------------------------------------------------------------------ */
/* 拖拽状态                                                            */
/* ------------------------------------------------------------------ */

/// 一次"按下—移动—抬起"的绘制过程。
struct Drag {
    active: bool,
    start: (i32, i32),
    last: (i32, i32),
    /// 本次拖拽开始前的整幅快照。只有形状工具需要它：每帧用它精确回滚
    /// 上一次预览的包围盒，而不是整幅重画。
    snapshot: Option<Vec<u8>>,
    /// 上一次预览的包围盒（下一帧要连同新包围盒一起回滚）。
    preview: Option<(i32, i32, i32, i32)>,
}

impl Drag {
    fn new() -> Drag {
        Drag {
            active: false,
            start: (0, 0),
            last: (0, 0),
            snapshot: None,
            preview: None,
        }
    }
}

/* ------------------------------------------------------------------ */
/* App                                                                 */
/* ------------------------------------------------------------------ */

pub struct App {
    sw: i32,
    sh: i32,

    title_bar: Obj,
    doc_size_label: Obj,

    menu_bar: Obj,
    menu_roots: [Obj; 3],
    menu_panels: [Obj; 3],
    menu_row_objs: [Obj; 3 * MENU_MAX_ROWS],
    menu_open: Option<usize>,

    work: Obj,
    tool_panel: Obj,
    tool_btns: [Obj; TOOL_COUNT],
    /// 每个工具格里的图标画布（ARGB8888 子对象，叠加在按钮之上）。
    tool_icons: [Obj; TOOL_COUNT],
    /// 对应图标的像素视图，用于切换选中态时重绘（换笔迹颜色）。
    tool_icon_docs: [Doc; TOOL_COUNT],
    tool: Tool,
    /// 每个工具按钮的**屏幕**矩形 (x, y, w, h)，悬停命中判定用。
    ///
    /// LVGL 9 没有 hover 状态（见 widget.rs 文件头），要判断"指针在某个
    /// 按钮上"只能自己拿全局指针坐标做矩形命中。把矩形在建按钮时一次性
    /// 算好存下来，比每拍去问 LVGL 对象几何便宜得多。
    tool_rects: [(i32, i32, i32, i32); TOOL_COUNT],

    /// 悬浮提示浮层。最后创建 ⇒ 在所有面板之上。
    tip_card: Obj,
    tip_title: Obj,
    tip_desc: Obj,
    tip_shown: bool,
    /// 当前悬停的工具序号；None 表示指针不在任何按钮上。
    hover_idx: Option<usize>,
    /// 本次悬停的起始时刻（`rp_now_ms`）。
    hover_since: u32,

    canvas: Obj,
    doc: Doc,
    canvas_x: i32,
    canvas_y: i32,
    doc_w: i32,
    doc_h: i32,

    palette: Obj,
    swatch_btns: [Obj; SWATCH_COUNT],
    color: u32,
    color_preview: Obj,

    status_tool: Obj,
    status_color: Obj,
    status_pos: Obj,
    status_size: Obj,
    status_ver: Obj,
    /// 状态栏上一次显示的坐标；`None` = 指针在画布外（此时显示 "- -"）。
    last_pos: Option<(i32, i32)>,

    drag: Drag,
    undo: Undo,

    dlg_scrim: Obj,
    dlg_card: Obj,
    dlg_btn: Obj,
    dlg_open: bool,

    quit: bool,
}

/// 全局单例。用裸指针而不是 `Option<App>` 静态量：既不制造对 `static mut`
/// 的引用，也不需要线程安全那套（UEFI 是单 CPU、单栈、协作式调度）。
static mut APP_PTR: *mut App = core::ptr::null_mut();

fn app() -> &'static mut App {
    unsafe { &mut *APP_PTR }
}

fn app_ready() -> bool {
    unsafe { !APP_PTR.is_null() }
}

/* ------------------------------------------------------------------ */
/* 对外入口（被 UefiMain.c 调用）                                       */
/* ------------------------------------------------------------------ */

pub fn build(image_handle: u64) -> i32 {
    let _ = image_handle;

    let mut sw = 0i32;
    let mut sh = 0i32;
    unsafe { ffi::rp_screen_size(&mut sw, &mut sh) };
    if sw < 320 || sh < 240 {
        ffi::log("screen too small, need >= 320x240");
        ffi::log_hex("screen w hi=", sw as u64);
        ffi::log_hex("screen h lo=", sh as u64);
        return 1;
    }

    let app_box = Box::new(App::prepare(sw, sh));
    unsafe { APP_PTR = Box::into_raw(app_box) };

    if !app().construct() {
        ffi::log("ui construct failed");
        return 2;
    }

    ffi::log("ui built");
    ffi::log_hex("screen packed=", ((sw as u64) << 16) | (sh as u64 & 0xFFFF));
    0
}

pub fn quit_requested() -> bool {
    if !app_ready() {
        return true;
    }
    app().quit
}

pub fn tick() {
    if !app_ready() {
        return;
    }
    app().on_tick();
}

pub fn destroy() {
    unsafe {
        if APP_PTR.is_null() {
            return;
        }
        // 撤销栈里是 MB 级的 Vec<u8>，不能靠进程退出兜底：UEFI pool 不随
        // 镜像退出回收，反复 run/exit 会累积（advmemtest 的 64KB 栈那条
        // 是同一类教训的另一面）。
        drop(Box::from_raw(APP_PTR));
        APP_PTR = core::ptr::null_mut();
    }
}

/* ------------------------------------------------------------------ */
/* 事件总入口                                                          */
/* ------------------------------------------------------------------ */

extern "C" fn trampoline(_obj: Obj, code: u32, key: u32, user: u64) {
    if !app_ready() {
        return;
    }
    app().dispatch(code, key, user);
}

impl App {
    fn prepare(sw: i32, sh: i32) -> App {
        App {
            sw,
            sh,
            title_bar: 0,
            doc_size_label: 0,
            menu_bar: 0,
            menu_roots: [0; 3],
            menu_panels: [0; 3],
            menu_row_objs: [0; 3 * MENU_MAX_ROWS],
            menu_open: None,
            work: 0,
            tool_panel: 0,
            tool_btns: [0; TOOL_COUNT],
            tool_icons: [0; TOOL_COUNT],
            tool_icon_docs: core::array::from_fn(|_| Doc::placeholder()),
            tool: Tool::Pencil,
            tool_rects: [(0, 0, 0, 0); TOOL_COUNT],
            tip_card: 0,
            tip_title: 0,
            tip_desc: 0,
            tip_shown: false,
            hover_idx: None,
            hover_since: 0,
            canvas: 0,
            doc: Doc::placeholder(),
            canvas_x: 0,
            canvas_y: 0,
            doc_w: 0,
            doc_h: 0,
            palette: 0,
            swatch_btns: [0; SWATCH_COUNT],
            color: 0x000000,
            color_preview: 0,
            status_tool: 0,
            status_color: 0,
            status_pos: 0,
            status_size: 0,
            status_ver: 0,
            last_pos: None,
            drag: Drag::new(),
            undo: Undo::new(),
            dlg_scrim: 0,
            dlg_card: 0,
            dlg_btn: 0,
            dlg_open: false,
            quit: false,
        }
    }

    /* ================= 构建 ================= */

    fn construct(&mut self) -> bool {
        let scr = unsafe { ffi::rp_screen() };
        if scr == 0 {
            return false;
        }
        unsafe { ffi::rp_set_bg(scr, BG_WINDOW, OPA_COVER) };

        let work_y = TITLE_H + MENU_H;
        let work_h = self.sh - TITLE_H - MENU_H - PALETTE_H - STATUS_H;
        if work_h < 120 {
            return false;
        }

        // ---- 工作区容器 ----
        self.work = widget::card(scr, 0, work_y, self.sw, work_h, BG_WINDOW, OPA_TRANS, 0);

        // ---- 画布几何：铺满工作区（扣掉左侧工具条与四周留白）----
        let area_x = TOOL_W;
        let area_w = self.sw - TOOL_W;
        let doc_w = (area_w - 2 * PAD).max(160);
        let doc_h = (work_h - 2 * PAD).max(120);
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        let rel_x = (area_w - doc_w) / 2;
        let rel_y = (work_h - doc_h) / 2;
        // 画布是 `work` 的子对象（work 在屏幕 (0, work_y)），所以 set_pos 给
        // 的是**相对父**的坐标，而 canvas_x/canvas_y 记录的是**屏幕**坐标。
        // 两者差一个 work 原点；rel_* 是"在 area 内居中"的余量，必须在
        // set_pos 那一步就把 area_x 加上。
        //
        // 这里踩过一次：只传 rel_x（漏了 area_x = TOOL_W = 56），画布被画在
        // 屏幕 x=8（左半截压在工具栏底下），而 canvas_x 仍按 64 参与换算
        // ⇒ 每次落笔都恒定左偏 56px。现在两处共用同一个 cv_x，结构上无法
        // 再分叉。
        let cv_x = area_x + rel_x;
        self.canvas_x = cv_x;
        self.canvas_y = work_y + rel_y;

        self.canvas = unsafe { ffi::rp_canvas_create(self.work, doc_w, doc_h) };
        if self.canvas == 0 {
            return false;
        }
        unsafe { ffi::rp_set_pos(self.canvas, cv_x, rel_y) };
        unsafe { ffi::rp_set_border(self.canvas, 1, BORDER_STRONG, OPA_COVER) };
        unsafe { ffi::rp_set_shadow(self.canvas, 12, theme::SHADOW, 26, 0, 3) };
        unsafe { ffi::rp_add_flag(self.canvas, ffi::FLAG_CLICKABLE) };
        unsafe { ffi::rp_on_event(self.canvas, trampoline, enc(UC_CANVAS, 0)) };

        match Doc::new(self.canvas) {
            Some(mut d) => {
                d.set_geometry(doc_w, doc_h);
                unsafe { ffi::rp_canvas_fill(self.canvas, 0xFFFFFF) };
                self.doc = d;
            }
            None => return false,
        }

        // ---- 标题栏 ----
        // 应用名保留拉丁（品牌名），中文信息一律用 CJK 字库。
        self.title_bar = widget::card(scr, 0, 0, self.sw, TITLE_H, BG_WINDOW, OPA_COVER, 0);
        widget::label_at(self.title_bar, 14, 9, "rustupaint", FONT_LARGE, TEXT_PRIMARY);
        let size_text = format!("画布 {} × {}", doc_w, doc_h);
        self.doc_size_label =
            widget::label_at(self.title_bar, 118, 12, &size_text, FONT_CJK_SMALL, TEXT_TERTIARY);
        // 右上角署名。混排中文+拉丁，所以整行走 CJK 字库（simsun 同时含
        // ASCII，字形风格统一）；右对齐交给 LVGL 排版，不估字符串宽度。
        widget::label_right(
            self.title_bar,
            self.sw - 16,
            12,
            220,
            AUTHOR_LINE,
            FONT_CJK_SMALL,
            TEXT_TERTIARY,
        );
        widget::hline(scr, 0, TITLE_H - 1, self.sw);

        // ---- 菜单栏 ----
        self.menu_bar = widget::card(scr, 0, TITLE_H, self.sw, MENU_H, BG_WINDOW, OPA_COVER, 0);
        for i in 0..MENU_NAMES.len() {
            let x = 8 + (i as i32) * MENU_ITEM_W;
            let item = widget::card(
                self.menu_bar,
                x,
                3,
                MENU_ITEM_W,
                MENU_H - 8,
                BG_WINDOW,
                OPA_TRANS,
                R_BUTTON,
            );
            widget::make_focusable(item, BG_WINDOW, BG_PRESSED, ACCENT_SOFT);
            widget::label_centered(item, MENU_NAMES[i], FONT_CJK, TEXT_PRIMARY);
            unsafe { ffi::rp_on_event(item, trampoline, enc(UC_MENU_ROOT, i as u32)) };
            self.menu_roots[i] = item;
        }
        widget::hline(scr, 0, TITLE_H + MENU_H - 1, self.sw);

        // ---- 左侧工具条 ----
        // 按钮是普通卡片 + 一张叠加的 ARGB 图标画布。图标**不是**按钮的
        // 文字，所以按钮的底色可以自由随状态变（常态/按下/焦点），图标始终
        // 正确透出——这正是画布要 ARGB 而不是 RGB 的原因。
        self.tool_panel = widget::card(self.work, 0, 0, TOOL_W, work_h, BG_PANEL, OPA_COVER, 0);
        widget::vline(self.work, TOOL_W - 1, 0, work_h);
        let btn_w = TOOL_W - 16;
        let icon_off = (btn_w - ICON_SIZE) / 2;
        for i in 0..TOOL_COUNT {
            let y = PAD + (i as i32) * (TOOL_BTN_H + 6);
            // 按钮在 tool_panel 里，tool_panel 在 work 里，work 在 (0, work_y)
            // —— 三层嵌套的屏幕坐标在这里一次算平，悬停判定直接用。
            self.tool_rects[i] = (PAD, work_y + y, btn_w, TOOL_BTN_H);
            let btn = widget::card(
                self.tool_panel,
                PAD,
                y,
                btn_w,
                TOOL_BTN_H,
                BG_PANEL,
                OPA_COVER,
                R_BUTTON,
            );
            widget::make_focusable(btn, BG_PANEL, BG_PRESSED, ACCENT_SOFT);
            unsafe { ffi::rp_on_event(btn, trampoline, enc(UC_TOOL, i as u32)) };

            let icon = unsafe { ffi::rp_canvas_create_argb(btn, ICON_SIZE, ICON_SIZE) };
            if icon != 0 {
                unsafe { ffi::rp_set_pos(icon, icon_off, (TOOL_BTN_H - ICON_SIZE) / 2) };
                // 图标画布必须不可点击，否则它会吃掉落在图标上的点击，
                // 事件到不了按钮（按钮才是注册了 UC_TOOL 的那个对象）。
                unsafe { ffi::rp_remove_flag(icon, ffi::FLAG_CLICKABLE) };
                match Doc::new(icon) {
                    Some(mut d) => {
                        d.set_geometry(ICON_SIZE, ICON_SIZE);
                        self.tool_icon_docs[i] = d;
                    }
                    None => ffi::log("icon canvas has no buffer"),
                }
            } else {
                ffi::log_hex("icon canvas create failed idx=", i as u64);
            }
            self.tool_btns[i] = btn;
            self.tool_icons[i] = icon;
        }
        self.highlight_tool();

        // ---- 底部调色板 ----
        //
        // 16 个色块排成**一行**。此前是两行各 8 个，但 PALETTE_H=44 装不下
        // （第一行 10..32、第二行 36..58），而 LVGL **默认不裁剪子对象**，
        // 于是第二行溢出容器、被后建的状态栏压住——实测屏幕 y=768..790 的
        // 那半行只露出 8px。单行既绕开了高度约束，也用上了窗口右侧的空白。
        let pal_y = self.sh - STATUS_H - PALETTE_H;
        self.palette = widget::card(scr, 0, pal_y, self.sw, PALETTE_H, BG_PANEL, OPA_COVER, 0);
        widget::hline(scr, 0, pal_y, self.sw);

        // 当前色预览：与色块同高、同一条水平中线
        let sw_y = (PALETTE_H - SWATCH_SIZE) / 2;
        let prev_w = 34;
        self.color_preview = widget::card(
            self.palette,
            PAD,
            sw_y,
            prev_w,
            SWATCH_SIZE,
            self.color,
            OPA_COVER,
            R_SWATCH,
        );
        unsafe { ffi::rp_set_border(self.color_preview, 1, BORDER_STRONG, OPA_COVER) };

        let sw_x0 = PAD + prev_w + 14;
        for i in 0..SWATCH_COUNT {
            let x = sw_x0 + (i as i32) * (SWATCH_SIZE + SWATCH_GAP);
            let sw = widget::card(
                self.palette,
                x,
                sw_y,
                SWATCH_SIZE,
                SWATCH_SIZE,
                SWATCHES[i],
                OPA_COVER,
                R_SWATCH,
            );
            widget::make_focusable(sw, SWATCHES[i], BG_PRESSED, SWATCHES[i]);
            // 白块/浅块在浅色底上没有描边会"消失"
            unsafe { ffi::rp_set_border(sw, 1, BORDER_STRONG, OPA_COVER) };
            unsafe { ffi::rp_on_event(sw, trampoline, enc(UC_SWATCH, i as u32)) };
            self.swatch_btns[i] = sw;
        }

        // ---- 状态栏 ----
        //
        // 整行统一用 FONT_CJK（SimSun 也含 ASCII，数字/十六进制照常显示）：
        // montserrat 上中文会**静默画不出来**，混排两套字库只会多一处出错点。
        let st_y = self.sh - STATUS_H;
        let st = widget::card(scr, 0, st_y, self.sw, STATUS_H, BG_WINDOW, OPA_COVER, 0);
        let tool0 = format!("工具：{}", self.tool.name());
        self.status_tool = widget::label_at(st, 12, 3, &tool0, FONT_CJK, TEXT_SECONDARY);
        self.status_color = widget::label_at(st, 110, 3, "颜色：#000000", FONT_CJK, TEXT_SECONDARY);
        self.status_pos = widget::label_at(st, 232, 3, "位置：x -, y -", FONT_CJK, TEXT_SECONDARY);
        let sz = format!("画布：{} x {} 像素", doc_w, doc_h);
        self.status_size = widget::label_at(st, 402, 3, &sz, FONT_CJK, TEXT_SECONDARY);
        // 版本串含构建时间戳（约 24 个 ASCII），按最宽情况留位，不逐帧量宽。
        let ver = format!("版本 {}", ffi::version());
        self.status_ver = widget::label_at(st, self.sw - 268, 3, &ver, FONT_CJK, TEXT_TERTIARY);

        // ---- 常驻浮层（最后创建 ⇒ 在最上层）----
        self.dlg_scrim = widget::card(scr, 0, 0, self.sw, self.sh, 0x000000, theme::SCRIM_OPA, 0);
        unsafe { ffi::rp_add_flag(self.dlg_scrim, ffi::FLAG_CLICKABLE) };
        unsafe { ffi::rp_add_flag(self.dlg_scrim, ffi::FLAG_HIDDEN) };

        self.build_about_dialog(scr);
        self.build_menu_panels(scr);
        self.build_tooltip(scr);

        self.rebuild_main_group();
        true
    }

    /// 工具栏悬浮提示浮层。
    ///
    /// 建在最后，所以它盖在菜单面板与对话框之上（LVGL 的层级就是创建顺序）。
    /// 初始 HIDDEN，靠 `update_tooltip` 每拍决定是否显形。
    fn build_tooltip(&mut self, scr: Obj) {
        let card = widget::floating_card(scr, TOOL_W + TOOLTIP_DX, 0, TOOLTIP_W, TOOLTIP_H, R_MENU);
        // 不进焦点组、不可点：tooltip 只是"看"的，不能抢 Tab 焦点，也不能
        // 挡住底下按钮的点击（LVGL 的点击命中会跳过 HIDDEN 对象，但不跳过
        // 可见对象，所以显式去掉 CLICKABLE）。
        unsafe { ffi::rp_set_pad(card, 0) };
        unsafe { ffi::rp_add_flag(card, ffi::FLAG_HIDDEN) };

        // 两行：标题是工具名，副行是一句用法。都用 CJK 字库 —— 中文在
        // montserrat 下会静默不画。
        self.tip_title = widget::label_at(card, 10, 5, "", FONT_CJK, TEXT_PRIMARY);
        self.tip_desc = widget::label_at(card, 10, 24, "", FONT_CJK_SMALL, TEXT_SECONDARY);
        self.tip_card = card;
    }

    /// 每拍推进悬停状态机：命中 → 计时 → 超时弹提示。
    fn update_tooltip(&mut self) {
        // 对话框开着时不弹：那时指针主要落在遮罩上，弹出来只会乱。
        if self.dlg_open {
            self.hide_tooltip();
            return;
        }

        let (mx, my) = self.screen_mouse();
        let hit = self
            .tool_rects
            .iter()
            .position(|r| mx >= r.0 && mx < r.0 + r.2 && my >= r.1 && my < r.1 + r.3);

        match hit {
            None => {
                self.hover_idx = None;
                self.hide_tooltip();
            }
            Some(i) => {
                let now = unsafe { ffi::rp_now_ms() };
                if self.hover_idx != Some(i) {
                    self.hover_idx = Some(i);
                    self.hover_since = now;
                    // 换按钮时先收起，避免旧提示跟着指针"跳"到新位置。
                    self.hide_tooltip();
                    return;
                }
                if now.wrapping_sub(self.hover_since) >= HOVER_DELAY_MS {
                    self.show_tooltip(i);
                }
            }
        }
    }

    fn show_tooltip(&mut self, i: usize) {
        let t = Tool::from_index(i as u32);
        let (_, by, _, bh) = self.tool_rects[i];
        // 垂直方向贴齐按钮中心，再按提示框高度抬一半。
        let y = (by + bh / 2 - TOOLTIP_H / 2).max(0);

        if !self.tip_shown {
            set_label(self.tip_title, t.name());
            set_label(self.tip_desc, t.tip());
            unsafe { ffi::rp_set_pos(self.tip_card, TOOL_W + TOOLTIP_DX, y) };
            unsafe { ffi::rp_remove_flag(self.tip_card, ffi::FLAG_HIDDEN) };
            self.tip_shown = true;
        }
    }

    fn hide_tooltip(&mut self) {
        if self.tip_shown {
            unsafe { ffi::rp_add_flag(self.tip_card, ffi::FLAG_HIDDEN) };
            self.tip_shown = false;
        }
    }

    fn build_menu_panels(&mut self, scr: Obj) {
        for m in 0..MENU_NAMES.len() {
            let rows = MENU_ROWS[m].len();
            let h = 8 + (rows as i32) * MENU_ROW_H;
            let x = 8 + (m as i32) * MENU_ITEM_W - 4;
            let y = TITLE_H + MENU_H;
            let panel = widget::floating_card(scr, x, y, MENU_PANEL_W, h, R_MENU);
            unsafe { ffi::rp_add_flag(panel, ffi::FLAG_HIDDEN) };

            for r in 0..rows {
                let (btn, _lb) = widget::text_button(
                    panel,
                    PAD,
                    4 + (r as i32) * MENU_ROW_H,
                    MENU_PANEL_W - 2 * PAD,
                    MENU_ROW_H - 2,
                    MENU_ROWS[m][r],
                    BG_CARD,
                    R_BUTTON,
                );
                unsafe {
                    ffi::rp_on_event(
                        btn,
                        trampoline,
                        enc(UC_MENU_ROW, (m * MENU_MAX_ROWS + r) as u32),
                    )
                };
                self.menu_row_objs[m * MENU_MAX_ROWS + r] = btn;
            }
            self.menu_panels[m] = panel;
        }
    }

    fn build_about_dialog(&mut self, scr: Obj) {
        let w = 448;
        // 224 -> 276：多出来的 52px 给"作者""邮箱"两行。
        let h = 276;
        let x = (self.sw - w) / 2;
        let y = (self.sh - h) / 2;
        let card = widget::floating_card(scr, x, y, w, h, R_CARD);
        unsafe { ffi::rp_add_flag(card, ffi::FLAG_HIDDEN) };

        // 中文一律 FONT_CJK / FONT_CJK_SMALL。用拉丁字库的话这些字会
        // **完全画不出来**（不是方框，是空白），而且不会有任何报错。
        widget::label_at(card, 24, 16, "关于 rustupaint", FONT_CJK, TEXT_PRIMARY);
        widget::hline(card, 0, 52, w);
        widget::label_at(
            card,
            24,
            68,
            "UEFI Shell 下的小画家。",
            FONT_CJK_SMALL,
            TEXT_PRIMARY,
        );
        widget::label_at(
            card,
            24,
            92,
            "逻辑用 Rust 编写，渲染由 LVGL 9.2.2 完成。",
            FONT_CJK_SMALL,
            TEXT_SECONDARY,
        );
        let v = format!("版本 {}", ffi::version());
        widget::label_at(card, 24, 116, &v, FONT_CJK_SMALL, TEXT_SECONDARY);
        // 作者/邮箱这两行是「中文标签 + 拉丁正文」的混排：
        // 整行只用 FONT_CJK_SMALL —— simsun 同时含 ASCII，两部分基线天生对齐；
        // 若把中文与 ASCII 拆成两个不同字库的 label，基线必然要手工对，
        // 而且换字号就崩。标点统一用全角冒号，与「版本 」那行的风格一致。
        widget::label_at(card, 24, 140, AUTHOR_LINE, FONT_CJK_SMALL, TEXT_SECONDARY);
        let mail = format!("邮箱：{}", AUTHOR_MAIL);
        widget::label_at(card, 24, 164, &mail, FONT_CJK_SMALL, TEXT_SECONDARY);
        widget::label_at(
            card,
            24,
            192,
            "Tab 切换焦点，Esc 关闭本对话框。",
            FONT_CJK_SMALL,
            TEXT_TERTIARY,
        );

        // 主按钮：强调色底 + 白字（Win11 的 primary button 观感）
        let ok = widget::card(card, w - 24 - 100, h - 24 - 36, 100, 36, ACCENT, OPA_COVER, R_BUTTON);
        widget::make_focusable(ok, ACCENT, 0x005BA1, ACCENT);
        widget::label_centered(ok, "确定", FONT_CJK, 0xFFFFFF);
        unsafe { ffi::rp_on_event(ok, trampoline, enc(UC_DLG, 0)) };

        self.dlg_card = card;
        self.dlg_btn = ok;
    }

    /* ================= 焦点组 ================= */

    /// 主界面 Tab 环：菜单 → 工具 → 画布 → 色板。
    /// 这是全工程唯一的"Tab 顺序定义"，改顺序只改这里。
    fn rebuild_main_group(&mut self) {
        unsafe { ffi::rp_group_clear() };
        for i in 0..MENU_NAMES.len() {
            unsafe { ffi::rp_group_add(self.menu_roots[i]) };
        }
        for i in 0..TOOL_COUNT {
            unsafe { ffi::rp_group_add(self.tool_btns[i]) };
        }
        unsafe { ffi::rp_group_add(self.canvas) };
        for i in 0..SWATCH_COUNT {
            unsafe { ffi::rp_group_add(self.swatch_btns[i]) };
        }
        unsafe { ffi::rp_group_focus(self.tool_btns[self.tool as usize]) };
    }

    fn enter_menu(&mut self, m: usize) {
        unsafe { ffi::rp_group_clear() };
        let rows = MENU_ROWS[m].len();
        let mut first = 0u64;
        for r in 0..rows {
            let o = self.menu_row_objs[m * MENU_MAX_ROWS + r];
            if o != 0 {
                unsafe { ffi::rp_group_add(o) };
                if first == 0 {
                    first = o;
                }
            }
        }
        if first != 0 {
            unsafe { ffi::rp_group_focus(first) };
        }
    }

    fn enter_dialog(&mut self) {
        unsafe { ffi::rp_group_clear() };
        unsafe { ffi::rp_group_add(self.dlg_btn) };
        unsafe { ffi::rp_group_focus(self.dlg_btn) };
    }

    /* ================= 菜单开合 ================= */

    fn open_menu(&mut self, m: usize) {
        self.close_menu();
        unsafe { ffi::rp_remove_flag(self.menu_panels[m], ffi::FLAG_HIDDEN) };
        self.menu_open = Some(m);
        self.enter_menu(m);
        ffi::log_hex("menu open idx=", m as u64);
    }

    fn close_menu(&mut self) {
        if let Some(m) = self.menu_open {
            unsafe { ffi::rp_add_flag(self.menu_panels[m], ffi::FLAG_HIDDEN) };
            self.menu_open = None;
            self.rebuild_main_group();
            unsafe { ffi::rp_group_focus(self.menu_roots[m]) };
        }
    }

    /* ================= 对话框 ================= */

    fn open_dialog(&mut self) {
        self.close_menu();
        unsafe { ffi::rp_remove_flag(self.dlg_scrim, ffi::FLAG_HIDDEN) };
        unsafe { ffi::rp_remove_flag(self.dlg_card, ffi::FLAG_HIDDEN) };
        self.dlg_open = true;
        self.enter_dialog();
        ffi::log("dialog about open");
    }

    fn close_dialog(&mut self) {
        unsafe { ffi::rp_add_flag(self.dlg_scrim, ffi::FLAG_HIDDEN) };
        unsafe { ffi::rp_add_flag(self.dlg_card, ffi::FLAG_HIDDEN) };
        self.dlg_open = false;
        self.rebuild_main_group();
        ffi::log("dialog about close");
    }

    /* ================= 命令 ================= */

    fn run_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::None => {}
            Cmd::New => {
                self.undo.clear();
                self.doc.fill(0xFFFFFF);
                self.refresh_canvas();
                ffi::log("cmd new");
            }
            Cmd::Clear => {
                self.undo.push(&self.doc);
                self.doc.fill(0xFFFFFF);
                self.refresh_canvas();
                ffi::log("cmd clear");
            }
            Cmd::Undo => {
                let ok = self.undo.pop_into(&mut self.doc);
                self.refresh_canvas();
                ffi::log_hex("cmd undo ok=", if ok { 1 } else { 0 });
            }
            Cmd::Exit => {
                self.quit = true;
                ffi::log("cmd exit");
            }
            Cmd::About => self.open_dialog(),
        }
    }

    /* ================= 工具与颜色 ================= */

    /// 把"当前选中工具"这个状态同时体现在**按钮底色**和**图标笔迹色**上：
    /// 选中 = 强调色底 + 强调色图标，其余 = 面板底色 + 主文本色图标。
    /// 两处一起变，选中的那个格子在任何背景下都一眼可见。
    fn highlight_tool(&mut self) {
        for i in 0..TOOL_COUNT {
            let t = Tool::from_index(i as u32);
            let selected = t == self.tool;
            let norm = if selected { ACCENT_SOFT } else { BG_PANEL };
            unsafe { ffi::rp_set_bg(self.tool_btns[i], norm, OPA_COVER) };

            let icon = self.tool_icons[i];
            if icon != 0 {
                let ink = if selected { ACCENT } else { TEXT_PRIMARY };
                icon::paint(t, &mut self.tool_icon_docs[i], ink);
                unsafe { ffi::rp_canvas_refresh(icon) };
            }
        }
    }

    fn select_tool(&mut self, idx: u32) {
        let t = Tool::from_index(idx);
        if t == Tool::Clear {
            // Clear 是一次性动作而非持续工具：执行后保持原工具不变，
            // 否则用户每次清屏后都要重新选一次画笔。
            self.run_cmd(Cmd::Clear);
            return;
        }
        self.tool = t;
        set_label(self.status_tool, &format!("工具：{}", t.name()));
        self.highlight_tool();
        ffi::log_hex("tool idx=", idx as u64);
    }

    fn set_color(&mut self, c: u32, from: &str) {
        self.color = c;
        unsafe { ffi::rp_set_bg(self.color_preview, c, OPA_COVER) };
        unsafe { ffi::rp_set_border(self.color_preview, 1, BORDER_STRONG, OPA_COVER) };
        set_label(self.status_color, &format!("颜色：#{:06X}", c));
        let _ = from;
    }

    /* ================= 画布交互 ================= */

    fn refresh_canvas(&mut self) {
        unsafe { ffi::rp_canvas_refresh(self.canvas) };
    }

    /// 指针的**屏幕**绝对坐标。命中判定（悬停、hit-test）一律用它。
    fn screen_mouse(&self) -> (i32, i32) {
        let mut x = 0i32;
        let mut y = 0i32;
        let mut valid = 0i32;
        unsafe { ffi::rp_mouse_pos(&mut x, &mut y, &mut valid) };
        let _ = valid;
        (x, y)
    }

    /// 指针换算到**画布像素**坐标（画布左上角为原点）。落笔用它。
    fn local_mouse(&self) -> (i32, i32) {
        let (x, y) = self.screen_mouse();
        (x - self.canvas_x, y - self.canvas_y)
    }

    fn clamp_x(&self, x: i32) -> i32 {
        x.max(0).min(self.doc_w - 1)
    }

    fn clamp_y(&self, y: i32) -> i32 {
        y.max(0).min(self.doc_h - 1)
    }

    fn begin_stroke(&mut self, rx: i32, ry: i32) {
        let x = self.clamp_x(rx);
        let y = self.clamp_y(ry);
        self.drag.active = true;
        self.drag.start = (x, y);
        self.drag.last = (x, y);

        match self.tool {
            Tool::Pencil => {
                self.undo.push(&self.doc);
                self.doc.stamp_round(x, y, 1, self.color);
            }
            Tool::Eraser => {
                self.undo.push(&self.doc);
                self.doc.stamp_round(x, y, ERASER_R, 0xFFFFFF);
            }
            Tool::Fill => {
                self.undo.push(&self.doc);
                self.doc.flood_fill(x, y, self.color);
            }
            Tool::Picker => {
                let c = self.doc.get_px(x, y);
                self.set_color(c, "picker");
                ffi::log_hex("picked=", c as u64);
            }
            Tool::Line | Tool::Rect | Tool::Ellipse => {
                // 形状工具：快照留给本帧起的"包围盒回滚"，抬起时才入撤销栈
                // （这样一次拖拽只持有一份快照，而不是两份）。
                self.drag.snapshot = Some(self.doc.snapshot());
                self.drag.preview = None;
            }
            Tool::Clear => {}
        }
        self.refresh_canvas();
    }

    fn continue_stroke(&mut self, rx: i32, ry: i32) {
        if !self.drag.active {
            return;
        }
        let x = self.clamp_x(rx);
        let y = self.clamp_y(ry);
        let (lx, ly) = self.drag.last;

        match self.tool {
            Tool::Pencil => {
                // 指针读数约 30Hz，快速划动时相邻采样点能差几十像素；不
                // 补插值线的话笔迹是一串断点。
                self.doc.line(lx, ly, x, y, self.color, 1);
                self.drag.last = (x, y);
            }
            Tool::Eraser => {
                self.doc.line(lx, ly, x, y, 0xFFFFFF, ERASER_R);
                self.drag.last = (x, y);
            }
            Tool::Line | Tool::Rect | Tool::Ellipse => {
                let (sx, sy) = self.drag.start;
                let cur = shape_bbox(sx, sy, x, y);
                let dirty = match self.drag.preview {
                    Some(p) => union(p, cur),
                    None => cur,
                };
                if let Some(snap) = self.drag.snapshot.as_ref() {
                    self.doc.restore_rect(dirty.0, dirty.1, dirty.2, dirty.3, snap);
                }
                match self.tool {
                    Tool::Line => self.doc.line(sx, sy, x, y, self.color, 1),
                    Tool::Rect => self.doc.rect_outline(sx, sy, x, y, self.color, 1),
                    Tool::Ellipse => self.doc.ellipse_outline(sx, sy, x, y, self.color, 1),
                    _ => {}
                }
                self.drag.preview = Some(cur);
                self.refresh_canvas();
            }
            Tool::Fill | Tool::Picker | Tool::Clear => {}
        }
        if matches!(self.tool, Tool::Pencil | Tool::Eraser) {
            self.refresh_canvas();
        }
    }

    fn end_stroke(&mut self) {
        if !self.drag.active {
            return;
        }
        if matches!(self.tool, Tool::Line | Tool::Rect | Tool::Ellipse) {
            if let Some(snap) = self.drag.snapshot.take() {
                // 预览已经落在画布上；把"拖拽前的样子"入撤销栈。
                self.undo.push_snapshot(snap);
            }
        }
        self.drag.active = false;
        self.drag.preview = None;
        self.refresh_canvas();
    }

    /* ================= 事件分发 ================= */

    fn dispatch(&mut self, code: u32, key: u32, user: u64) {
        let (class, idx) = dec(user);

        // ---- 全局键处理：先于类别分发，任何控件都能触发 ----
        if code == ffi::EV_KEY {
            match key {
                ffi::KEY_TAB => {
                    unsafe { ffi::rp_group_focus_next() };
                    return;
                }
                ffi::KEY_TAB_PREV => {
                    unsafe { ffi::rp_group_focus_prev() };
                    return;
                }
                ffi::KEY_ESC => {
                    if self.dlg_open {
                        self.close_dialog();
                    } else if self.menu_open.is_some() {
                        self.close_menu();
                    }
                    return;
                }
                ffi::KEY_UP | ffi::KEY_DOWN => {
                    // 只在浮层里用上下键移动焦点：主界面上方向键若无故抢走
                    // 焦点，用户会以为键盘坏了。主界面唯一的焦点键是 Tab。
                    if self.menu_open.is_some() || self.dlg_open {
                        if key == ffi::KEY_UP {
                            unsafe { ffi::rp_group_focus_prev() };
                        } else {
                            unsafe { ffi::rp_group_focus_next() };
                        }
                    }
                    return;
                }
                _ => {}
            }
        }

        // 菜单开着时点别处 = 收起菜单（点菜单项本身除外）
        if self.menu_open.is_some() && class != UC_MENU_ROW && class != UC_MENU_ROOT {
            if code == ffi::EV_PRESSED {
                self.close_menu();
            }
        }

        match class {
            UC_CANVAS => match code {
                ffi::EV_PRESSED => {
                    let p = self.local_mouse();
                    self.begin_stroke(p.0, p.1);
                }
                ffi::EV_PRESSING => {
                    let p = self.local_mouse();
                    self.continue_stroke(p.0, p.1);
                }
                ffi::EV_RELEASED => self.end_stroke(),
                _ => {}
            },
            UC_MENU_ROOT => {
                if is_activate(code, key) {
                    match self.menu_open {
                        Some(m) if m == idx as usize => self.close_menu(),
                        _ => self.open_menu(idx as usize),
                    }
                }
            }
            UC_MENU_ROW => {
                if is_activate(code, key) {
                    let m = (idx as usize) / MENU_MAX_ROWS;
                    let r = (idx as usize) % MENU_MAX_ROWS;
                    let cmd = menu_cmd(m, r);
                    self.close_menu();
                    self.run_cmd(cmd);
                }
            }
            UC_TOOL => {
                if is_activate(code, key) {
                    self.select_tool(idx);
                }
            }
            UC_SWATCH => {
                if is_activate(code, key) {
                    let c = SWATCHES[idx as usize];
                    self.set_color(c, "swatch");
                }
            }
            UC_DLG => {
                if is_activate(code, key) {
                    self.close_dialog();
                }
            }
            _ => {}
        }
    }

    /* ================= 每拍 ================= */

    fn on_tick(&mut self) {
        self.update_tooltip();

        let (x, y) = self.local_mouse();
        // 指针移出画布要把坐标还原成 "-"，而不是冻在最后一个值上——
        // 否则状态栏会显示一个"鼠标早就不在那儿"的陈旧坐标。
        let shown = if self.doc.in_bounds(x, y) {
            Some((x, y))
        } else {
            None
        };
        if shown != self.last_pos {
            self.last_pos = shown;
            match shown {
                Some((x, y)) => set_label(self.status_pos, &format!("位置：x {}, y {}", x, y)),
                None => set_label(self.status_pos, "位置：x -, y -"),
            }
        }
    }
}

/* ------------------------------------------------------------------ */
/* 小工具                                                              */
/* ------------------------------------------------------------------ */

/// 把一个 Rust 字符串写进**已有**标签。
///
/// 与 `widget::label_at`（建新标签）相对。LVGL 侧对 text 是拷贝，所以这里的
/// 定长临时缓冲用完即可丢——不必自己管生命周期。
fn set_label(obj: Obj, text: &str) {
    if obj == 0 {
        return;
    }
    let cs = ffi::cstr(text);
    unsafe { ffi::rp_set_text(obj, cs.as_ptr() as *const core::ffi::c_char) };
}

/// 点击、或在焦点项上按回车，都算"激活"。键盘用户按回车 = 点它。
fn is_activate(code: u32, key: u32) -> bool {
    code == ffi::EV_CLICKED || (code == ffi::EV_KEY && key == ffi::KEY_ENTER)
}

/// 形状工具当前预览的包围盒（留笔宽余量，保证回滚范围盖得住画出来的线）。
fn shape_bbox(sx: i32, sy: i32, x: i32, y: i32) -> (i32, i32, i32, i32) {
    const PADB: i32 = 3;
    (
        sx.min(x) - PADB,
        sy.min(y) - PADB,
        sx.max(x) + PADB,
        sy.max(y) + PADB,
    )
}

fn union(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}
