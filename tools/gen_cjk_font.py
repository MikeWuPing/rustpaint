#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Regenerate the LVGL 16px SimSun CJK font so it covers the UI's Chinese text.

WHY THIS EXISTS
---------------
The CJK font shipped inside LvglPkg (`Fonts/lv_font_simsun_16_cjk.c`) is a
*subset*: ~4400 codepoints, and its CJK part is a near-random 691 characters
inherited from whatever `--symbols` list the upstream converter run happened to
use. A character that is not in the subset renders as **nothing at all** -- no
box, no placeholder -- so a Chinese UI built on that font shows silently blank
labels. Verified: 文 / 件 / 编 / 铅 / 笔 / 橡 / 皮 are all absent from it.

WHAT IT DOES
------------
1. Reads the `Opts:` line out of the existing font .c, so whatever the current
   subset already contains is PRESERVED -- this is an *additive* regeneration,
   never a replacement. (Same approach as the earlier "additive" font work in
   the advmemtest tree.)
2. Scans rust/src/*.rs for double-quoted string LITERALS and collects every
   non-ASCII character in them. Comments are deliberately skipped: the doc
   comments are full of Chinese that never reaches the screen, and including
   them would bloat the font for nothing.
3. Appends those codepoints to the SimSun clause of the converter command line.
4. Rewrites the `--font` paths to this machine's copies of the source fonts.
5. Runs `node lv_font_conv` and then VERIFIES the result by re-parsing the
   generated cmaps and checking that every character it was asked for is
   actually present. A silent subset miss is exactly the failure mode this
   whole script exists to prevent, so the verification is not optional.

All argv is ASCII on purpose: this machine's console code page is GBK, and
non-ASCII argv gets silently re-encoded on the way to node. Characters are
therefore always passed as `-r <decimal codepoint list>`, never as `--symbols
<chars>`.

Usage (from the project root):
    python tools/gen_cjk_font.py            # regenerate in place
    python tools/gen_cjk_font.py --check    # verify only, change nothing
"""
import argparse
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

FONT_C = os.path.join(ROOT, 'LvglPkg', 'Library', 'LvglLib', 'Fonts',
                      'lv_font_simsun_16_cjk.c')
# Source fonts live in the LVGL tree that ships with the workspace's LvglPkg.
SRC_DIR = os.path.join(ROOT, 'LvglPkg', 'Library', 'LvglLib', 'lvgl',
                       'scripts', 'built_in_font')
SIMSUN_WOFF = os.path.join(SRC_DIR, 'SimSun.woff')
FA_WOFF = os.path.join(SRC_DIR, 'FontAwesome5-Solid+Brands+Regular.woff')
SEGUISYM = r'C:\Windows\Fonts\seguisym.ttf'
LV_FONT_CONV = os.path.join(
    os.environ.get('APPDATA', ''), 'npm', 'node_modules', 'lv_font_conv',
    'lv_font_conv.js')

# Extra characters to always include, on top of whatever the Rust sources use.
# These are the words a UEFI Shell paint app is likely to grow into, so a later
# edit does not silently produce a blank label before the font is regenerated.
# Cheap insurance: ~60 glyphs at 16px/bpp4 is well under 10 KB.
EXTRA_CHARS = (
    # UI vocabulary likely to appear next
    "新建打开保存另存为关闭退出撤销重做清空全选复制粘贴删除"
    "铅笔橡皮擦直线矩形椭圆圆形填充油漆桶取色吸管工具颜色调色板"
    "画布位置坐标像素尺寸大小宽高当前就绪状态版本关于帮助文件编辑视图"
    "确定取消是否应用重置默认设置选项菜单工具栏状态栏标题栏滚动条"
    "提示错误失败成功警告信息请输入选择请输入文件名路径"
    # full-width / CJK punctuation a Chinese UI will want.
    # (Curly quotes ‘ ’ “ ” are deliberately NOT listed: this SimSun subset does
    # not carry them, and the UI does not use them -- listing them would make
    # --check permanently red for characters that never reach the screen.)
    "、。，：；！？（）《》——…·"
)


def rust_ui_chars():
    """Non-ASCII characters inside double-quoted string literals of rust/src."""
    src_dir = os.path.join(ROOT, 'rust', 'src')
    found = set()
    for name in sorted(os.listdir(src_dir)):
        if not name.endswith('.rs'):
            continue
        text = open(os.path.join(src_dir, name), encoding='utf-8').read()
        # Strip line comments first so their Chinese does not leak in.
        text = re.sub(r'//[^\n]*', '', text)
        for lit in re.findall(r'"((?:[^"\\]|\\.)*)"', text):
            for ch in lit:
                if ord(ch) > 0x7F:
                    found.add(ch)
    return found


def read_opts(font_c):
    text = open(font_c, encoding='utf-8', errors='replace').read()
    m = re.search(r'^\s*\*\s*Opts:\s*(.+?)\s*$', text, re.M)
    if not m:
        sys.exit('FATAL: no "Opts:" line found in ' + font_c)
    return m.group(1).split()


def build_argv(opts):
    """opts -> argv with this machine's font paths and our extra codepoints."""
    # Split into clauses at each --font; the leading tokens are global options.
    clauses, head, cur = [], [], None
    for tok in opts:
        if tok == '--font':
            cur = []
            clauses.append(cur)
            continue
        (head if cur is None else cur).append(tok)
    if not clauses:
        sys.exit('FATAL: no --font clause in the Opts line')

    argv = list(head)
    for ci, clause in enumerate(clauses):
        path = clause[0]
        if 'SimSun' in path:
            path = SIMSUN_WOFF
        elif 'FontAwesome' in path:
            path = FA_WOFF
        elif path.endswith('seguisym.ttf'):
            path = SEGUISYM
        argv += ['--font', path]
        argv += clause[1:]
    return argv


def gen_argv_for_chars(argv, codepoints):
    """Insert `-r <cps>` into the SimSun clause (the first non-FA clause)."""
    out = []
    inserted = False
    i = 0
    while i < len(argv):
        tok = argv[i]
        out.append(tok)
        if tok == '--font':
            out.append(argv[i + 1])
            # The SimSun clause is the one whose path is our SimSun woff.
            if not inserted and argv[i + 1] == SIMSUN_WOFF:
                out += ['-r', ','.join(str(c) for c in codepoints)]
                inserted = True
            i += 2
            continue
        i += 1
    if not inserted:
        sys.exit('FATAL: could not find the SimSun clause to add glyphs to')
    return out


# ---------------------------------------------------------------- verification
# Reuse the one correct cmap parser rather than keeping a second copy here.
# (An earlier duplicate in this file read SPARSE `unicode_list` entries as
# absolute codepoints when they are offsets from range_start, which made a font
# that already had every needed glyph look like it was missing 131 of them.
# One implementation, one place to be right.)
sys.path.insert(0, HERE)
from font_coverage import coverage as parse_coverage  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--check', action='store_true',
                    help='verify the current font covers the UI text; do not rebuild')
    ap.add_argument('--out', default='',
                    help='write the generated .c somewhere else (default: in place)')
    args = ap.parse_args()

    if not os.path.exists(FONT_C):
        sys.exit('FATAL: font not found: ' + FONT_C)

    wanted = rust_ui_chars() | set(EXTRA_CHARS)
    cps = sorted({ord(c) for c in wanted})
    print('characters required by the UI: %d' % len(wanted))
    print('  sample: ' + ''.join(chr(c) for c in cps[:60]))

    covered = parse_coverage(FONT_C)
    missing = [c for c in cps if c not in covered]
    print('current font covers %d codepoints; missing %d of the required %d'
          % (len(covered), len(missing), len(cps)))
    if not missing:
        print('OK: every character the UI uses is present.')
        return 0
    print('  missing: ' + ''.join(chr(c) for c in missing[:120]))

    if args.check:
        print('--check: not rebuilding.')
        return 1

    # ------------------------------------------------------------ regenerate
    if not os.path.exists(LV_FONT_CONV):
        sys.exit('FATAL: lv_font_conv.js not found at ' + LV_FONT_CONV +
                 '\n  install it with: npm i -g lv_font_conv')
    for p in (SIMSUN_WOFF, FA_WOFF, SEGUISYM):
        if not os.path.exists(p):
            sys.exit('FATAL: source font missing: ' + p)

    argv = build_argv(read_opts(FONT_C))
    argv = gen_argv_for_chars(argv, cps)

    out_c = args.out or FONT_C
    k = argv.index('-o')
    argv[k + 1] = out_c
    bad = [a for a in argv if not a.isascii()]
    if bad:
        sys.exit('FATAL: non-ASCII in argv (console code page would mangle it): %r' % bad)

    # -o must be the last thing lv_font_conv sees; its parser wants the file
    # name after the flags it belongs to. Keep the original ordering.
    print('running lv_font_conv (%d argv tokens) -> %s' % (len(argv), out_c))
    r = subprocess.run(['node', LV_FONT_CONV] + argv,
                       capture_output=True, text=True)
    if r.stdout.strip():
        print(r.stdout.strip()[:1500])
    if r.returncode != 0:
        print(r.stderr.strip()[:3000], file=sys.stderr)
        sys.exit('FATAL: lv_font_conv failed with exit %d' % r.returncode)

    # ------------------------------------------------------------ verify
    covered2 = parse_coverage(out_c)
    missing2 = [c for c in cps if c not in covered2]
    print('regenerated font covers %d codepoints' % len(covered2))
    if missing2:
        sys.exit('FATAL: %d characters STILL missing after regeneration: %s'
                 % (len(missing2), ''.join(chr(c) for c in missing2[:120])))
    kept = sum(1 for c in covered if c in covered2)
    print('verified: all %d required characters present' % len(cps))
    print('preserved %d of the %d codepoints the old font had' % (kept, len(covered)))
    return 0


if __name__ == '__main__':
    sys.exit(main())
