#!/usr/bin/env python3
"""笔迹落点像素校验 —— 证明"鼠标点哪儿、笔迹就在哪儿"。

背景：画布曾因 rp_set_pos 漏加 area_x（=TOOL_W=56）而被画在屏幕 x=8，
但坐标换算仍按 canvas_x=64 ⇒ 每笔恒定左偏 56px。肉眼只能说"好像偏了"，
没法量化，所以这里直接读截图像素。

两种模式：

  区间模式 —— 在 (x0,y0)..(x1,y1) 所在屏幕行上量墨迹的左右端点：
      python tools/check_stroke.py <png> <x0> <y0> <x1> <y1>

  点模式（更严格）—— 逐点检查"该像素附近有没有墨"，用于验证单击落点。
  QMP 的相对位移注入有累积误差，所以拖动轨迹的两端本就不该拿来做
  亚像素判定；单击那种"零位移"用例才是干净的判据：
      python tools/check_stroke.py <png> --dots 300,200 500,300 700,450
"""
import sys

try:
    from PIL import Image
except ImportError:
    sys.exit("Pillow required: py -m pip install Pillow")


def any_ink_near(px, w, h, cx, cy, rad=3):
    """(cx, cy) 半径 rad 内是否存在非白像素。"""
    for y in range(cy - rad, cy + rad + 1):
        if not (0 <= y < h):
            continue
        for x in range(cx - rad, cx + rad + 1):
            if not (0 <= x < w):
                continue
            r, g, b = px[x, y]
            if r < 200 or g < 200 or b < 200:
                return True
    return False


def check_dots(path, dots, rad=3):
    im = Image.open(path).convert("RGB")
    w, h = im.size
    px = im.load()
    print(f"image      : {path}  ({w}x{h})")
    bad = 0
    for (cx, cy) in dots:
        ok = any_ink_near(px, w, h, cx, cy, rad)
        # 顺带看 56px 左侧（历史偏移的指纹）有没有墨——若目标点没墨而这里
        # 有墨，就是那个 bug 复发了。
        ghost = any_ink_near(px, w, h, cx - 56, cy, rad)
        print(f"dot ({cx},{cy}): {'INK' if ok else 'MISSING'}"
              + (f"   [ghost ink at x={cx-56}!]" if ghost and not ok else ""))
        if not ok:
            bad += 1
    print("VERDICT    : " + ("PASS - every dot landed under its cursor"
                             if bad == 0 else f"FAIL - {bad} dot(s) missing"))
    return 0 if bad == 0 else 1


def main():
    if len(sys.argv) >= 4 and sys.argv[2] == "--dots":
        dots = [tuple(map(int, s.split(","))) for s in sys.argv[3:]]
        return check_dots(sys.argv[1], dots)

    if len(sys.argv) != 6:
        sys.exit(__doc__)
    path, x0, y0, x1, y1 = sys.argv[1], *map(int, sys.argv[2:])
    im = Image.open(path).convert("RGB")
    w, h = im.size
    px = im.load()

    # 取笔画中点所在的那一屏行，向上下各扫 2 行容忍抗锯齿与线宽。
    ymid = (y0 + y1) // 2
    lo, hi = None, None
    for y in range(ymid - 2, ymid + 3):
        if not (0 <= y < h):
            continue
        for x in range(w):
            r, g, b = px[x, y]
            # 白底画布：任何明显偏离白的像素都算"有笔迹"
            if r < 200 or g < 200 or b < 200:
                lo = x if lo is None else min(lo, x)
                hi = x if hi is None else max(hi, x)

    exp_lo, exp_hi = min(x0, x1), max(x0, x1)
    print(f"image      : {path}  ({w}x{h})")
    print(f"expected   : ink span x={exp_lo}..{exp_hi}  (row y={ymid})")
    if lo is None:
        print("measured   : NO INK FOUND")
        return 1
    print(f"measured   : ink span x={lo}..{hi}")
    d0, d1 = lo - exp_lo, hi - exp_hi
    print(f"delta      : left {d0:+d}px, right {d1:+d}px")
    ok = abs(d0) <= 3 and abs(d1) <= 3
    # 56 = TOOL_W，正是历史上那个偏移量的指纹。单独标出来便于一眼识别。
    if not ok and abs(d0 + 56) <= 3:
        print("VERDICT    : FAIL - offset matches the TOOL_W=56 signature "
              "(canvas pos vs canvas_x desync)")
    else:
        print("VERDICT    : " + ("PASS - ink lands under the cursor" if ok
                                 else "FAIL - unexplained offset"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
