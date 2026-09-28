//! Win11 风格的主题令牌（颜色 / 尺寸 / 圆角 / 阴影）。
//!
//! 单一真源：所有界面代码的取色与尺寸都从这里取，不要在别处硬编码
//! 颜色字面量——否则"改一档主题"会变成满地找数字。
//!
//! 取色依据 Windows 11 浅色主题的实际观感：底 #F3F3F3（近似 mica）、
//! 卡片 #FFFFFF、描边 #E5E5E5、主文本 #1B1B1B、次文本 #616161、
//! 强调色 #0067C0（Win11 默认 accent 的近似值）。

#![allow(dead_code)]

/* ---------------- 颜色 ---------------- */

/// 窗口底 / mica 近似色。标题栏、菜单栏、状态栏同色。
pub const BG_WINDOW: u32 = 0xF3F3F3;
/// 卡片 / 面板底（画布纸张、对话框）。
pub const BG_CARD: u32 = 0xFFFFFF;
/// 次级面板底（工具栏、调色板）。
pub const BG_PANEL: u32 = 0xFAFAFA;
/// 悬停 / 选中态的浅底。
pub const BG_HOVER: u32 = 0xEBEBEB;
/// 按下态。
pub const BG_PRESSED: u32 = 0xE0E0E0;

/// 描边（Win11 的 1px 内描边很浅）。
pub const BORDER: u32 = 0xE5E5E5;
/// 强描边（对话框、焦点有描边需求时）。
pub const BORDER_STRONG: u32 = 0xD0D0D0;

/// 主文本。
pub const TEXT_PRIMARY: u32 = 0x1B1B1B;
/// 次文本（状态栏、说明）。
pub const TEXT_SECONDARY: u32 = 0x616161;
/// 禁用 / 提示。
pub const TEXT_TERTIARY: u32 = 0x9A9A9A;

/// 强调色（选中工具、焦点圈、主按钮）。
pub const ACCENT: u32 = 0x0067C0;
/// 强调色的浅底（选中项背景）。
pub const ACCENT_SOFT: u32 = 0xD6E7F8;

/// 阴影颜色（Win11 的浮层阴影很淡）。
pub const SHADOW: u32 = 0x000000;
pub const SHADOW_OPA: u32 = 38;

/// 模态遮罩的不透明度（0..255）。
pub const SCRIM_OPA: u32 = 72;

/* ---------------- 尺寸 ---------------- */

pub const TITLE_H: i32 = 36;
pub const MENU_H: i32 = 32;
pub const TOOL_W: i32 = 56;
pub const TOOL_BTN_H: i32 = 40;
pub const PALETTE_H: i32 = 44;
pub const STATUS_H: i32 = 24;
pub const PAD: i32 = 8;

/// 色块边长。
///
/// 16 个色块排成**一行**（不是两行）：单行在 1280 宽的窗口里有充足横向
/// 空间，而且能避免此前那个坑——`PALETTE_H` 装不下两行时，LVGL 默认不裁剪
/// 子对象，第二行会溢出容器并被后建的状态栏压住（实测只露出 8px）。
/// 单行 + 垂直居中，容器高度就不再是约束。
pub const SWATCH_SIZE: i32 = 26;
pub const SWATCH_GAP: i32 = 6;

/* ---------------- 圆角 ---------------- */

pub const R_WINDOW: i32 = 0;
pub const R_CARD: i32 = 8;
pub const R_BUTTON: i32 = 4;
pub const R_MENU: i32 = 6;
pub const R_SWATCH: i32 = 3;

/* ---------------- 不透明度 ---------------- */

pub const OPA_FULL: u32 = 255;
/// 焦点圈透明度。
pub const FOCUS_OPA: u32 = 255;

/* ---------------- 调色板 ---------------- */

/// 16 个色板（Win11 Paint 的基础色一行近似）。
pub const SWATCHES: [u32; 16] = [
    0x000000, // black
    0x7F7F7F, // gray
    0x9A9A9A, // light gray
    0xFFFFFF, // white
    0x7F0000, // dark red
    0xE81123, // red
    0xFF8C00, // orange
    0xFFD400, // yellow
    0x107C10, // green
    0x00B294, // teal
    0x00B7C3, // cyan
    0x0067C0, // blue
    0x2B1A9A, // indigo
    0x8E44AD, // purple
    0xC239B3, // magenta
    0x8B5E3C, // brown
];

/// 工具标识。索引即 `App::tool` 的取值。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pencil = 0,
    Eraser = 1,
    Line = 2,
    Rect = 3,
    Ellipse = 4,
    Fill = 5,
    Picker = 6,
    Clear = 7,
}

pub const TOOL_COUNT: usize = 8;

impl Tool {
    /// 状态栏用的中文名字。
    ///
    /// 工具栏本身不再用文字（改成图标，见 `icon.rs`），但状态栏要有一行
    /// "当前工具：铅笔" 之类，所以名字仍然需要一个真源。
    ///
    /// 这些字必须都在 SimSun 字库里——montserrat 没有 CJK 字形，中文会
    /// **静默画不出来**（连方框都没有）。改这里的文案后跑一次
    /// `python tools/gen_cjk_font.py --check` 复核。
    pub fn name(self) -> &'static str {
        match self {
            Tool::Pencil => "铅笔",
            Tool::Eraser => "橡皮",
            Tool::Line => "直线",
            Tool::Rect => "矩形",
            Tool::Ellipse => "椭圆",
            Tool::Fill => "填充",
            Tool::Picker => "取色",
            Tool::Clear => "清空",
        }
    }

    /// 悬浮提示的第二行：一句话说清这个工具干什么。
    ///
    /// 工具栏是纯图标的（见 `icon.rs`），图标再象形也不如一行字明确，所以
    /// 鼠标停在按钮上超过阈值就弹 tooltip，标题用 `name()`、这句作补充。
    ///
    /// 同样受字库约束：改文案后跑 `python tools/gen_cjk_font.py --check`。
    pub fn tip(self) -> &'static str {
        match self {
            Tool::Pencil => "按住拖动自由手绘",
            Tool::Eraser => "按住拖动擦成白色",
            Tool::Line => "按住拖动画直线",
            Tool::Rect => "按住拖动画矩形",
            Tool::Ellipse => "按住拖动画椭圆",
            Tool::Fill => "点一下填满同色区域",
            Tool::Picker => "点一下拾取该点颜色",
            Tool::Clear => "清空整幅画布",
        }
    }

    /// Clear 是"一次性动作"而不是"持续工具"：选中即执行，随后回到
    /// 上一个持续工具。绘制类工具都返回 true。
    pub fn is_sticky(self) -> bool {
        !matches!(self, Tool::Clear)
    }

    pub fn from_index(i: u32) -> Tool {
        match i {
            0 => Tool::Pencil,
            1 => Tool::Eraser,
            2 => Tool::Line,
            3 => Tool::Rect,
            4 => Tool::Ellipse,
            5 => Tool::Fill,
            6 => Tool::Picker,
            _ => Tool::Clear,
        }
    }
}
