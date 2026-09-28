#!/usr/bin/env python3
"""把若干张 QEMU 截图串成一个 GIF（产品手册的"操作演示"用）。

为什么不用 ffmpeg：这里的输入是**同尺寸、无压缩需求**的少数几帧，
Pillow 直接写 GIF 就够了，而且能逐帧指定时长（操作停顿比等间隔更像
真人操作）。

用法：
    python tools/make_gif.py out.gif [--size 720] [--dwell 1400] \
        frame1.png:900 frame2.png:2200 frame3.png

每个参数是 `<文件>[:时长ms]`，省略时长则用 --dwell。
"""
import argparse
import sys

try:
    from PIL import Image
except ImportError:
    sys.exit("Pillow required: py -m pip install Pillow")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("frames", nargs="+", help="path.png[:duration_ms]")
    ap.add_argument("--size", type=int, default=720,
                    help="输出宽度，等比缩放（默认 720，控制 GIF 体积）")
    ap.add_argument("--dwell", type=int, default=1400,
                    help="未标注时长时的默认帧时长（ms）")
    ap.add_argument("--loop", type=int, default=0, help="0 = 无限循环")
    a = ap.parse_args()

    imgs, durs = [], []
    for spec in a.frames:
        path, _, ms = spec.rpartition(":")
        if not path:            # 没有冒号 ⇒ 整个都是路径
            path, ms = spec, ""
        im = Image.open(path).convert("RGB")
        if a.size and im.width != a.size:
            h = int(round(im.height * a.size / im.width))
            im = im.resize((a.size, h), Image.LANCZOS)
        # 调色板量化：GIF 每帧最多 256 色，直接 convert("P") 在截图这种
        # 大量抗锯齿灰阶上会出现色带，所以用 ADAPTIVE + 抖动。
        imgs.append(im.convert("P", palette=Image.ADAPTIVE, colors=256))
        durs.append(int(ms) if ms else a.dwell)

    imgs[0].save(a.out, save_all=True, append_images=imgs[1:],
                 duration=durs, loop=a.loop, optimize=True, disposal=2)
    total = sum(durs)
    print(f"wrote {a.out}: {len(imgs)} frame(s), {a.size}px wide, "
          f"{total/1000:.1f}s total, {max(durs)}ms longest")


if __name__ == "__main__":
    sys.exit(main())
