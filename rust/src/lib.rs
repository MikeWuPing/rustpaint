//! rustupaint —— UEFI Shell 下的小画家。
//!
//! 本 crate 编成**静态库**（`crate-type = ["staticlib"]`），由 EDK2 的
//! VS2019 链接器与 C 侧的 UefiMain.c / RpShim.c 一起链成 `rustupaint.efi`。
//! 因此这里**没有 main**，只有四个导出给 C 的入口：
//!
//! | 导出符号        | 调用者          | 职责                       |
//! |-----------------|-----------------|----------------------------|
//! | `rp_app_build`  | UefiMain.c      | 建界面，返回 0 表示成功     |
//! | `rp_app_quit`   | UefiMain.c 每拍 | 主循环退出条件             |
//! | `rp_app_tick`   | UefiMain.c 每拍 | 每拍刷新（状态栏等）        |
//! | `rp_app_destroy`| UefiMain.c 退出 | 释放 Rust 侧持有的内存      |
//!
//! 运行环境是 `no_std`：UEFI 固件里没有 libc、没有文件系统抽象、没有
//! 线程、没有 panic unwind。所以本文件还负责三件"托底"的事：
//! 全局分配器接到 UEFI pool、panic 出口、以及禁止 unwind 的行为约定。

#![no_std]
#![allow(dead_code)]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

mod app;
mod canvas;
mod ffi;
mod icon;
mod theme;
mod widget;

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::{c_char, c_void};
use core::fmt::Write as _;

/* ------------------------------------------------------------------ */
/* 全局分配器：Rust 的堆就是 UEFI 的 pool                               */
/* ------------------------------------------------------------------ */

/// UEFI `AllocatePool` 的对齐保证。x86_64 上绝大多数类型的对齐要求都
/// 不超过它，所以下面的快速路径覆盖了几乎全部实际分配。
const POOL_ALIGN: usize = 8;

struct RpAllocator;

unsafe impl GlobalAlloc for RpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        if align <= POOL_ALIGN {
            return ffi::rp_alloc(size as u64) as *mut u8;
        }

        // 超出 pool 对齐保证：多要一段，把原始指针记在对齐块正前方。
        // 这样 dealloc 即使只拿到对齐后的指针也能找回要还给谁。
        let total = size + align + core::mem::size_of::<usize>();
        let raw = ffi::rp_alloc(total as u64) as usize;
        if raw == 0 {
            return core::ptr::null_mut();
        }
        let start = raw + core::mem::size_of::<usize>();
        let aligned = (start + align - 1) & !(align - 1);
        *((aligned as *mut usize).offset(-1)) = raw;
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        if layout.align() <= POOL_ALIGN {
            ffi::rp_free(ptr as *mut c_void);
        } else {
            // 对齐块正前方存的是 rp_alloc 返回的原始指针。
            let raw = *((ptr as *mut usize).offset(-1));
            ffi::rp_free(raw as *mut c_void);
        }
    }

    // realloc 不实现：GlobalAlloc 的默认实现（新分配 + 拷贝 + 释放）在本
    // 工程里既正确又够用。刻意不接 C 侧的 rp_realloc —— 那会让"对齐 >
    // POOL_ALIGN 的分配"的簿记逻辑出现第二条路径，两条路径迟早不一致。
}

#[global_allocator]
static RP_ALLOCATOR: RpAllocator = RpAllocator;

/* ------------------------------------------------------------------ */
/* panic 出口                                                          */
/* ------------------------------------------------------------------ */

/// 定长 ASCII 缓冲，用于把 panic 消息送到串口。
///
/// 刻意不用 `format!`：panic 可能发生在分配器已经出错的时候，此时再去
/// 申请内存只会掩盖现场。因此这里只做"逐字节写进栈上的数组"。
struct FmtBuf {
    buf: [u8; 192],
    len: usize,
}

impl FmtBuf {
    fn new() -> FmtBuf {
        FmtBuf {
            buf: [0; 192],
            len: 0,
        }
    }
}

impl core::fmt::Write for FmtBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.as_bytes() {
            if self.len >= self.buf.len() - 1 {
                break;
            }
            // 串口通道是 ASCII 的（真机控制台没有 CJK 字模），非 ASCII
            // 一律折成 '?'，宁可读起来丑也不要输出乱码字节。
            self.buf[self.len] = if *b < 0x80 { *b } else { b'?' };
            self.len += 1;
        }
        Ok(())
    }
}

#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    let mut w = FmtBuf::new();
    let _ = write!(w, "PANIC: {}", info.message());
    w.buf[w.len] = 0;
    unsafe { ffi::rp_panic(w.buf.as_ptr() as *const c_char) }
}

/* ------------------------------------------------------------------ */
/* 导出给 C 侧的入口                                                    */
/* ------------------------------------------------------------------ */

/// 建界面。0 = 成功。
#[no_mangle]
pub extern "C" fn rp_app_build(image_handle: u64) -> i32 {
    app::build(image_handle)
}

/// 主循环退出条件（非 0 = 该退了）。
#[no_mangle]
pub extern "C" fn rp_app_quit() -> i32 {
    if app::quit_requested() {
        1
    } else {
        0
    }
}

/// 主循环每拍调用。
#[no_mangle]
pub extern "C" fn rp_app_tick() {
    app::tick()
}

/// 退出前释放 Rust 侧持有的内存。
#[no_mangle]
pub extern "C" fn rp_app_destroy() {
    app::destroy()
}
