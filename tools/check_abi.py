#!/usr/bin/env python3
"""RpShim.h <-> ffi.rs 符号一致性校验。

背景：RpShim.h 与 rust/src/ffi.rs 是一对必须同步的手写声明。CLAUDE.md 只立了
"改一边必须改另一边" 的纪律，但没有任何工具保障 —— 加一个 rp_* 函数或 RP_*
常量而忘了改 ffi.rs，编译期未必报错（函数漏声明要等到链接期，常量漏声明则
根本不报错，只是值悄悄错位）。这个脚本把纪律变成可执行的检查。

约定（当前工程的映射规则）：
  - 函数：同名，rp_foo -> fn rp_foo
  - 常量：C 侧 RP_EV_PRESSED -> Rust 侧 EV_PRESSED（剥掉 RP_ 前缀），数值必须相等

用法：
    python tools/check_abi.py            # 检查，有漂移则以非零码退出
    python tools/check_abi.py --verbose   # 连一致的符号也列出来

退出码：0 = 一致；1 = 有漂移；2 = 文件找不到。
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HEADER = os.path.join(ROOT, "RustPaintPkg", "Application", "RustPaint", "RpShim.h")
FFI = os.path.join(ROOT, "rust", "src", "ffi.rs")

# C 侧 #define，值可能是十进制、十六进制、或带括号的简单表达式
RE_C_DEFINE = re.compile(r"^#define\s+([A-Z][A-Z0-9_]*)\s+(.+?)\s*$", re.M)
# Rust 侧 pub const
RE_RS_CONST = re.compile(r"^pub const ([A-Z][A-Z0-9_]*)\s*:\s*(\w+)\s*=\s*([^;]+);", re.M)
# C 侧函数声明：函数名在行首或跟随返回类型，取 rp_ 开头的标识符
RE_C_FN = re.compile(r"\b(rp_[a-z0-9_]+)\s*\(")
# Rust 侧 extern 声明
RE_RS_FN = re.compile(r"\bfn\s+(rp_[a-z0-9_]+)\s*\(")

# C 侧不需要在 Rust 侧有对应物的宏
IGNORE_DEFINES = {"RP_SHIM_H_"}


def strip_c_comments(text):
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    text = re.sub(r"//[^\n]*", "", text)
    return text


def strip_rs_comments(text):
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    text = re.sub(r"//[^\n]*", "", text)
    return text


def parse_int(raw):
    """把 C/Rust 侧的字面量解析成 int；解析不了返回 None（跳过比较）。"""
    s = raw.strip().strip("()").strip()
    s = s.replace("U", "").replace("u", "").replace("L", "")
    s = s.rstrip("uUiIlL")
    try:
        return int(s, 0)
    except ValueError:
        return None


def main():
    verbose = "--verbose" in sys.argv
    for path in (HEADER, FFI):
        if not os.path.exists(path):
            print("missing: %s" % path)
            return 2

    with open(HEADER, encoding="utf-8", errors="replace") as f:
        c_text = strip_c_comments(f.read())
    with open(FFI, encoding="utf-8", errors="replace") as f:
        rs_text = strip_rs_comments(f.read())

    # --- 函数 ---
    c_fns = set(RE_C_FN.findall(c_text))
    rs_fns = set(RE_RS_FN.findall(rs_text))

    # --- 常量 ---
    c_consts = {}
    for name, raw in RE_C_DEFINE.findall(c_text):
        if name in IGNORE_DEFINES:
            continue
        c_consts[name] = parse_int(raw)
    rs_consts = {}
    for name, _ty, raw in RE_RS_CONST.findall(rs_text):
        rs_consts[name] = parse_int(raw)

    problems = []

    fn_only_c = sorted(c_fns - rs_fns)
    fn_only_rs = sorted(rs_fns - c_fns)
    if fn_only_c:
        problems.append("fn: declared in C but not Rust -> %s" % ", ".join(fn_only_c))
    if fn_only_rs:
        problems.append("fn: declared in Rust but not C -> %s" % ", ".join(fn_only_rs))

    const_missing = []
    const_mismatch = []
    for name, val in sorted(c_consts.items()):
        rs_name = name[len("RP_"):] if name.startswith("RP_") else name
        if rs_name not in rs_consts:
            const_missing.append("%s (期望 Rust 侧 %s)" % (name, rs_name))
            continue
        if val is None or rs_consts[rs_name] is None:
            continue
        if val != rs_consts[rs_name]:
            const_mismatch.append("%s = %s vs %s = %s" % (name, val, rs_name, rs_consts[rs_name]))
    if const_missing:
        problems.append("const: missing on Rust side -> %s" % ", ".join(const_missing))
    if const_mismatch:
        problems.append("const: value mismatch -> %s" % "; ".join(const_mismatch))

    rs_extra = sorted(n for n in rs_consts if ("RP_" + n) not in c_consts and n not in c_consts)
    if rs_extra:
        problems.append("const: on Rust side only -> %s" % ", ".join(rs_extra))

    print("RpShim.h    : %d functions, %d constants" % (len(c_fns), len(c_consts)))
    print("ffi.rs      : %d functions, %d constants" % (len(rs_fns), len(rs_consts)))
    print("functions matched    : %d" % len(c_fns & rs_fns))
    print("constants compared   : %d" % sum(1 for n in c_consts
                                   if (n[len("RP_"):] if n.startswith("RP_") else n) in rs_consts))
    print()
    if problems:
        for p in problems:
            print("[DRIFT] %s" % p)
        return 1
    print("OK: RpShim.h and ffi.rs agree")
    if verbose:
        print("\n--- functions ---")
        for n in sorted(c_fns):
            print("  %s" % n)
        print("\n--- constants ---")
        for n in sorted(c_consts):
            rs_name = n[len("RP_"):] if n.startswith("RP_") else n
            print("  %-24s -> %-20s = %s" % (n, rs_name, c_consts[n]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
