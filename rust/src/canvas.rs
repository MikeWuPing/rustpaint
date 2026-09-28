//! 画布文档模型与绘制原语。
//!
//! 本模块**只做像素**，不认识任何 UI 概念（对象句柄只在 `new` 里用来
//! 取一次缓冲与跨距，之后就只认裸指针与几何）。这样画笔算法可以脱离
//! LVGL 单独推演，也便于将来做单元测试。
//!
//! 像素格式固定 RGB888（3 字节/像素，字节序 R,G,B）。**行偏移必须用
//! `stride`**，不是 `w * 3` —— LVGL 的行可能带对齐填充，用 w*3 画出来
//! 的图会逐行斜移（这是 UEFI 图形编程的经典坑）。

use alloc::vec::Vec;

use crate::ffi::{self, Obj};

/// 撤销栈深度上限。每层是一份完整画布快照（约 stride*h 字节）。
pub const UNDO_MAX: usize = 8;

/// 0x00RRGGBB → (r, g, b)
#[inline]
fn split(rgb: u32) -> (u8, u8, u8) {
    (
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    )
}

/// (r, g, b) → 0x00RRGGBB
#[inline]
pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// 两个颜色按 0..=255 的比例做线性混合（用于画笔的抗锯齿边缘与软橡皮）。
#[inline]
fn blend(dst: u32, src: u32, alpha: u32) -> u32 {
    if alpha == 0 {
        return dst;
    }
    if alpha >= 255 {
        return src;
    }
    let (dr, dg, db) = split(dst);
    let (sr, sg, sb) = split(src);
    let a = alpha;
    let ia = 255 - alpha;
    let r = ((sr as u32 * a + dr as u32 * ia) / 255) as u8;
    let g = ((sg as u32 * a + dg as u32 * ia) / 255) as u8;
    let b = ((sb as u32 * a + db as u32 * ia) / 255) as u8;
    rgb(r, g, b)
}

/// 画布文档。
///
/// `bpp` 是每像素字节数：主画布是 3（RGB888），工具栏图标是 4（ARGB8888）。
/// 两种都要支持，是因为图标必须带 alpha——不透明底会盖住按钮自己的按下/
/// 焦点底色。**行偏移用 `stride`，不用 `w*bpp`**（见模块头注释）。
pub struct Doc {
    buf: *mut u8,
    w: i32,
    h: i32,
    stride: i32,
    bpp: u8,
}

impl Doc {
    /// 空占位文档（缓冲为空）。用于 `App` 构造前的字段初始化 ——
    /// 真正的 Doc 在画布对象建好之后由 `Doc::new` 填进来。
    pub fn placeholder() -> Doc {
        Doc {
            buf: core::ptr::null_mut(),
            w: 0,
            h: 0,
            stride: 0,
            bpp: 3,
        }
    }

    /// 从 C 侧刚建好的画布取缓冲与几何。返回 None 表示画布没建成功。
    ///
    /// 每像素字节数**问 C 侧**（`rp_canvas_bpp`）而不是靠调用方声明：建的是
    /// ARGB 却按 RGB 写会让整幅图逐像素错位，而且错得很像"画歪了"而不是
    /// "格式不对"，很难查。一个查询换掉一整类 bug。
    pub fn new(canvas: Obj) -> Option<Doc> {
        let buf = unsafe { ffi::rp_canvas_buf(canvas) } as *mut u8;
        let stride = unsafe { ffi::rp_canvas_stride(canvas) };
        let bpp = unsafe { ffi::rp_canvas_bpp(canvas) };
        if buf.is_null() || stride <= 0 || (bpp != 3 && bpp != 4) {
            return None;
        }
        // 画布的宽高由创建时的参数决定，C 侧已经把跨距算好了；这里反推
        // 不出宽高，由调用方通过 `set_geometry` 补上（见 app.rs）。
        Some(Doc {
            buf,
            w: 0,
            h: 0,
            stride,
            bpp: bpp as u8,
        })
    }

    pub fn set_geometry(&mut self, w: i32, h: i32) {
        self.w = w;
        self.h = h;
    }

    #[inline]
    pub fn w(&self) -> i32 {
        self.w
    }

    #[inline]
    pub fn h(&self) -> i32 {
        self.h
    }

    #[inline]
    pub fn stride(&self) -> i32 {
        self.stride
    }

    #[inline]
    pub fn bpp(&self) -> u8 {
        self.bpp
    }

    /// 单像素字节偏移。
    #[inline]
    fn off(&self, x: i32, y: i32) -> isize {
        (y as isize) * (self.stride as isize) + (x as isize) * (self.bpp as isize)
    }

    #[inline]
    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.w && y < self.h
    }

    /// 直写一个像素（不做边界检查 —— 调用方必须自己判 in_bounds）。
    ///
    /// # Safety
    /// 由 `in_bounds` 的前置条件保证；本函数是 private。
    #[inline]
    fn put_unchecked(&mut self, x: i32, y: i32, c: u32) {
        let o = self.off(x, y);
        let (r, g, b) = split(c);
        unsafe {
            // LVGL 的 RGB888 与 ARGB8888 在内存里**都是 B,G,R(,A)** —— 不是
            // 名字暗示的 R,G,B。依据是 LVGL 自己的实现，不是推断：
            //   lv_draw_sw_blend_to_rgb888.c:
            //       dest_buf_u8[x + 0] = dsc->color.blue;
            //       dest_buf_u8[x + 2] = dsc->color.red;
            //   misc/lv_color.h:
            //       typedef struct { uint8_t blue, green, red, alpha; } lv_color32_t;
            //
            // 顺序写反**不报错、不越界**，只是每个颜色通道错位：蓝变橙、
            // 红蓝互换。而纯黑/纯白笔迹完全看不出来（灰度是对称的），所以
            // 这个 bug 一直藏到"画彩色的那一刻"才会暴露——实测就是画了一笔
            // 红色才发现是蓝的。
            *self.buf.offset(o) = b;
            *self.buf.offset(o + 1) = g;
            *self.buf.offset(o + 2) = r;
            if self.bpp == 4 {
                // alpha 固定 255：显式写像素即"要画出东西"。透明只来自
                // 初始状态或 clear_transparent()。
                *self.buf.offset(o + 3) = 255;
            }
        }
    }

    /// 读回一个像素，返回 `0x00RRGGBB`（与写入时同一套约定）。
    #[inline]
    fn get_unchecked(&self, x: i32, y: i32) -> u32 {
        let o = self.off(x, y);
        unsafe {
            rgb(
                *self.buf.offset(o + 2),
                *self.buf.offset(o + 1),
                *self.buf.offset(o),
            )
        }
    }

    /// 把整幅缓冲清零。ARGB 画布上这等于"全透明"（A=0），
    /// RGB 画布上等于全黑 —— 所以只在重绘图标前调用。
    pub fn clear_transparent(&mut self) {
        let n = (self.stride as usize) * (self.h as usize);
        if self.buf.is_null() || n == 0 {
            return;
        }
        unsafe {
            core::ptr::write_bytes(self.buf, 0, n);
        }
    }

    pub fn set_px(&mut self, x: i32, y: i32, c: u32) {
        if self.in_bounds(x, y) {
            self.put_unchecked(x, y, c);
        }
    }

    pub fn get_px(&self, x: i32, y: i32) -> u32 {
        if self.in_bounds(x, y) {
            self.get_unchecked(x, y)
        } else {
            0
        }
    }

    /// 方形笔尖：以 (cx,cy) 为中心盖一个 (2r+1)² 的实心块。
    /// 铅笔用 r=1（3×3），橡皮用 r=4（9×9）。solid=255 为全覆盖。
    pub fn stamp(&mut self, cx: i32, cy: i32, r: i32, c: u32, alpha: u32) {
        for y in (cy - r)..=(cy + r) {
            for x in (cx - r)..=(cx + r) {
                if self.in_bounds(x, y) {
                    if alpha >= 255 {
                        self.put_unchecked(x, y, c);
                    } else {
                        let d = self.get_unchecked(x, y);
                        self.put_unchecked(x, y, blend(d, c, alpha));
                    }
                }
            }
        }
    }

    /// 圆形笔尖（半径 r 的实心圆盘）——比方形笔尖的笔迹更自然。
    pub fn stamp_round(&mut self, cx: i32, cy: i32, r: i32, c: u32) {
        let rr = (r * r) as i64;
        for dy in -r..=r {
            for dx in -r..=r {
                if (dx * dx + dy * dy) as i64 <= rr {
                    let x = cx + dx;
                    let y = cy + dy;
                    if self.in_bounds(x, y) {
                        self.put_unchecked(x, y, c);
                    }
                }
            }
        }
    }

    pub fn fill(&mut self, c: u32) {
        for y in 0..self.h {
            for x in 0..self.w {
                self.put_unchecked(x, y, c);
            }
        }
    }

    pub fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u32) {
        let (xa, xb) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
        let (ya, yb) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
        for y in ya..=yb {
            for x in xa..=xb {
                if self.in_bounds(x, y) {
                    self.put_unchecked(x, y, c);
                }
            }
        }
    }

    /// Bresenham 直线（带方形笔尖，r=0 时即 1px 线）。
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u32, r: i32) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let (mut x, mut y) = (x0, y0);

        loop {
            self.stamp(x, y, r, c, 255);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// 矩形描边（四条线；不用 fill_rect 减内框，避免 r 变化时出现断角）。
    pub fn rect_outline(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u32, r: i32) {
        self.line(x0, y0, x1, y0, c, r);
        self.line(x1, y0, x1, y1, c, r);
        self.line(x1, y1, x0, y1, c, r);
        self.line(x0, y1, x0, y0, c, r);
    }

    /// 不带小数的整数平方根（Newton 迭代）。
    ///
    /// no_std 的 UEFI 目标下没有 libm，`f64::sqrt` 不可用；而椭圆描边
    /// 只需要求根到整数精度，所以自带一个。输入为负时返回 0。
    fn isqrt(n: i64) -> i64 {
        if n <= 0 {
            return 0;
        }
        let mut x = n;
        let mut y = (x + 1) / 2;
        while y < x {
            x = y;
            y = (x + n / x) / 2;
        }
        x
    }

    /// 椭圆描边。
    ///
    /// 用**解析扫描**而不是增量中点算法：对每个 x 求两个 y、再对每个 y
    /// 求两个 x，两趟叠加就得到闭合轮廓（换向的那一段由第二趟补上）。
    /// 代价是 O(rx+ry) 次整数开方，对画布尺寸而言可以忽略；换来的是
    /// "闭不闭合"这件事可以直接推演，不必依赖增量决策参数的推导不出错。
    /// 这是刻意用一点点性能换掉一整类"椭圆缺个口/多一块"的调试。
    pub fn ellipse_outline(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u32, r: i32) {
        let xa = x0.min(x1);
        let xb = x0.max(x1);
        let ya = y0.min(y1);
        let yb = y0.max(y1);

        let cx = (xa + xb) / 2;
        let cy = (ya + yb) / 2;
        let rx = (xb - xa) / 2;
        let ry = (yb - ya) / 2;

        if rx <= 0 || ry <= 0 {
            // 退化成线或点
            self.line(xa, ya, xb, yb, c, r);
            return;
        }

        let rx2 = (rx as i64) * (rx as i64);
        let ry2 = (ry as i64) * (ry as i64);

        // 趟 1：沿 x 扫（覆盖顶部/底部平缓段）
        for dx in -rx..=rx {
            let t = rx2 - (dx as i64) * (dx as i64);
            let dy = if t <= 0 { 0 } else { Self::isqrt(ry2 * t / rx2) };
            self.stamp(cx + dx, cy + dy as i32, r, c, 255);
            self.stamp(cx + dx, cy - dy as i32, r, c, 255);
        }

        // 趟 2：沿 y 扫（覆盖左右两侧陡峭段，把可能的缺口补掉）
        for dy in -ry..=ry {
            let t = ry2 - (dy as i64) * (dy as i64);
            let dx = if t <= 0 { 0 } else { Self::isqrt(rx2 * t / ry2) };
            self.stamp(cx + dx as i32, cy + dy, r, c, 255);
            self.stamp(cx - dx as i32, cy + dy, r, c, 255);
        }
    }

    /// 油漆桶：扫描线洪水填充（栈显式，避免递归爆栈 —— UEFI 的
    /// guard 之外没有可用的栈保护，递归写在这里迟早要出事）。
    pub fn flood_fill(&mut self, sx: i32, sy: i32, new_color: u32) -> usize {
        if !self.in_bounds(sx, sy) {
            return 0;
        }
        let target = self.get_unchecked(sx, sy);
        if target == new_color {
            return 0;
        }

        let mut stack: Vec<(i32, i32)> = Vec::new();
        stack.push((sx, sy));
        let mut painted = 0usize;

        while let Some((x, y)) = stack.pop() {
            if !self.in_bounds(x, y) || self.get_unchecked(x, y) != target {
                continue;
            }
            // 向左右扩到边界
            let mut left = x;
            while left - 1 >= 0 && self.get_unchecked(left - 1, y) == target {
                left -= 1;
            }
            let mut right = x;
            while right + 1 < self.w && self.get_unchecked(right + 1, y) == target {
                right += 1;
            }
            // 涂这一行，并把上下两行的待办压栈
            for px in left..=right {
                self.put_unchecked(px, y, new_color);
                painted += 1;
            }
            for ny in [y - 1, y + 1] {
                if ny < 0 || ny >= self.h {
                    continue;
                }
                let mut px = left;
                while px <= right {
                    if self.get_unchecked(px, ny) == target {
                        stack.push((px, ny));
                        // 跳过本段连续可填区域，避免同一段反复入栈
                        while px <= right && self.get_unchecked(px, ny) == target {
                            px += 1;
                        }
                    } else {
                        px += 1;
                    }
                }
            }
        }
        painted
    }

    /// 整幅快照（撤销用）。
    pub fn snapshot(&self) -> Vec<u8> {
        let n = (self.stride as usize) * (self.h as usize);
        let mut v = Vec::with_capacity(n);
        unsafe {
            core::ptr::copy_nonoverlapping(self.buf, v.as_mut_ptr(), n);
            v.set_len(n);
        }
        v
    }

    /// 用快照覆盖整幅。
    pub fn restore_all(&mut self, snap: &[u8]) {
        let n = ((self.stride as usize) * (self.h as usize)).min(snap.len());
        unsafe {
            core::ptr::copy_nonoverlapping(snap.as_ptr(), self.buf, n);
        }
    }

    /// 只把矩形区域还原回快照内容 —— 形状工具做实时预览的基石：
    /// 每帧只回滚"上一次预览的包围盒 ∪ 本次预览的包围盒"，而不是整幅
    /// 覆盖。整幅覆盖在全屏画布上是每帧 1MB 级的内存拷贝，QEMU 下会
    /// 直接把交互拖垮。
    pub fn restore_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, snap: &[u8]) {
        let xa = x0.max(0).min(self.w - 1);
        let xb = x1.max(0).min(self.w - 1);
        let ya = y0.max(0).min(self.h - 1);
        let yb = y1.max(0).min(self.h - 1);
        if xa > xb || ya > yb {
            return;
        }
        let row_bytes = ((xb - xa + 1) as usize) * 3;
        for y in ya..=yb {
            let off = (y as isize) * (self.stride as isize) + (xa as isize) * 3;
            if off < 0 || off as usize + row_bytes > snap.len() {
                continue;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(snap.as_ptr().offset(off), self.buf.offset(off), row_bytes);
            }
        }
    }
}

/// 撤销栈（有界）。
pub struct Undo {
    stack: Vec<Vec<u8>>,
}

impl Undo {
    pub fn new() -> Undo {
        Undo { stack: Vec::new() }
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// 入栈一份快照。超出深度上限时丢最旧的一层。
    pub fn push(&mut self, doc: &Doc) {
        if self.stack.len() >= UNDO_MAX {
            self.stack.remove(0);
        }
        self.stack.push(doc.snapshot());
    }

    /// 直接入栈一份已有的快照（形状工具用：拖拽期间那份快照既做实时
    /// 预览的回滚源，又在抬起时成为撤销层 —— 一次拖拽只持有一份，
    /// 而不是"预览一份 + 撤销再拷一份"）。
    pub fn push_snapshot(&mut self, snap: Vec<u8>) {
        if self.stack.len() >= UNDO_MAX {
            self.stack.remove(0);
        }
        self.stack.push(snap);
    }

    /// 撤销一次。返回 false 表示没有可撤销的内容。
    pub fn pop_into(&mut self, doc: &mut Doc) -> bool {
        match self.stack.pop() {
            Some(snap) => {
                doc.restore_all(&snap);
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }
}
