"""QMP driver for the closed loop: screendump frames + pointer/keyboard input.

Usage:
  qmp_drive.py --port 4555 --shot-dir snapshots --interval 1 --max-shots 60
               --script "t2 ret t3 up up up t2 screendump main"
  t<N>      sleep N seconds
  <key>     sendkey (QEMU qcode name: up/down/left/right/ret/esc/spc/backspace/f2/f5/ctrl_r/...)
  <key>xN   sendkey N times with 300ms spacing (no typematic on QEMU PS/2)
  screendump NAME   capture framebuffer to <shot-dir>/<stamp>_NAME.png
  vnc|HOST|PORT|CMD,...   spawn tools/vnc_wheel.py (VNC pointer/wheel injection)
               with commas as spaces -- e.g.
               vnc|127.0.0.1|5901|move,400,300,wheel,down,8
               (QEMU 10.2 QMP has no wheel axis; RFB button4/5 is the channel.
               Requires the VM to be launched with -vnc, i.e. Run script -VncPort.)
  rel|DX|DY    relative pointer move via QMP input-send-event (rel x/y axes)
               -- the channel for RELATIVE mice (fallback SimplePointer path);
               keep |DX|,|DY| <= 127 (HID boot-mouse report range).
  hover|X|Y    move the pointer to guest screen pixel (X,Y), deterministically:
               bang into the top-left corner with repeated large negative rel
               (driver side clamps -> exact (0,0) reset), then walk there with
               calibrated <=127-unit steps. No -VncPort needed (QMP only).
               Calibration constants below; see HOVER_* comments.
  btn|left     mouse button edge (press+release). Carries NO coordinates: the
               pointer stays exactly where the preceding hover|X|Y left it, so
               "aim" (hover) and "click" (btn) stay decoupled. This is the
               reliable click channel -- a VNC PointerEvent carries absolute
               coords which QEMU converts to a relative delta for usb-mouse and
               the HID boot report clamps at +/-127, so a large jump lands
               off-target (theme_switch_probe.py measured a request of
               (470,390) landing at (645,407)). Idiom: hover|X|Y t1 btn|left.
               Other button names are passed through to QMP's InputButton
               (middle/right/wheel-up/wheel-down/...).
  down|left / up|left   press-and-HOLD / release. With rel| in between this is
               how you drag out a stroke:
                 hover|X|Y t1 down|left t1 rel|80|40 t1 rel|80|40 t1 up|left
               Use rel| for the drag, never hover| -- hover starts by banging
               the pointer into the corner, which while the button is held draws
               a spurious line back to the origin.

Output format: frames are written as PNG. QEMU's screendump does NOT reliably
pick the format from the extension (10.2.50 still writes a P6 PPM for a `.png`
name), so the magic bytes are sniffed and a Pillow conversion is done only when
needed. Pillow is therefore optional; without it the raw PPM is kept.

WALL-CLOCK SAFETY: --deadline bounds the scripted phase, and a daemon thread
forces os._exit shortly after, so this process can never wedge a caller that
runs it synchronously.

Exit code 0 on clean completion, 1 on QMP error.
"""
import argparse, json, socket, struct, subprocess, sys, threading, time, os

# Pillow 是可选的：QEMU >= 7.1 的 screendump 按**文件扩展名**选格式，
# 直接要 .png 就得到 PNG，不需要任何图像库。本机的 managed Python
# (C:\Users\mikew\.workbuddy\binaries\python) 并没有装 Pillow，硬 import
# 会让驱动在第一行就退出 —— 表象是"QMP 日志里什么都没有、snapshot 是空的"。
# 只有回退到 .ppm 时才需要它。
try:
    from PIL import Image  # noqa: F401
    _HAVE_PIL = True
except ImportError:
    Image = None
    _HAVE_PIL = False

# ---- 悬停注入标定（Task B，2026-09-13）--------------------------------
# 通道：QMP input-send-event 的 rel 轴 → QEMU usb-mouse（boot 协议相对鼠标）
# → 固件 UsbMouseAbsolutePointerDxe 把每份 HID 报告的位移累加进
#   0..AbsoluteMax 的绝对窗口 → LvglUefiPort 的 AbsolutePointer 分支再按
#   screen = Current * 屏宽 / AbsoluteMax 线性映射成像素
#   （LvglPkg/Library/LvglUefiPort/InputMouse.c，MouseReadCb）。
#
# 实测（本机 QEMU 10.2 + firmware/OVMF_CODE.fd + -vga std 1280x800）：
#   * 驱动量程 AbsoluteMax X/Y = 1024（EDK2 UsbMouseAbsolutePointer.c:672-673）；
#   * **1 个 rel 单位 = 1 个设备单位** —— 串口 `[LvglPort] ptr raw=X,Y`
#     回读：rel|100|0 后 raw 恰好 +100（merge 掉的话会是 127 的整数倍）；
#   * 故 1 个 rel 单位 = 1280/1024 = 1.25 px（X）、800/1024 = 0.78125 px（Y）；
#   * 撞角：连续负位移后 raw 恒为 0,0（驱动钳位），是确定性的复位动作。
# 用例只写屏幕像素；换固件/换分辨率/换映射公式时动这里（constants + CLI）。
HOVER_HID_MAX   = 127    # HID boot 鼠标单份报告的位移上限（QEMU 超出截断）
HOVER_STEP      = 120    # 走位步长：< 上限，留出合并余量（>127 会丢余量）
HOVER_SETTLE    = 0.06   # 每步静默：> usb-mouse 10ms 中断间隔 + 固件处理
HOVER_BANG_N    = 16     # 撞角事件数（512 单位即触底；16×127 留 4 倍余量）
HOVER_EDGE_WAIT = 0.3    # 撞角→走位的静默：防最后一批负位移与新步并成一份报告

def hard_exit_after(seconds):
    """最后一道闸门：守护线程到点直接 os._exit。

    --deadline 是主循环里的礼貌退出（会先收尾、打印），但它只在循环顶部
    被检查——如果某一次调用自己卡住（QMP 读、screendump 轮询、PIL 转换），
    礼貌退出永远等不到。本脚本被 Run 脚本**同步**调用，一次卡死就等于整个
    构建-运行循环停摆（实测过：run 卡了 4 分钟以上、QEMU 还在跑、版本断言
    根本没执行）。守护线程 + os._exit 保证进程一定结束，不给调用方留隐患。
    """
    def worker():
        time.sleep(seconds)
        print(f"[qmp] HARD deadline {seconds}s hit; forcing exit", flush=True)
        os._exit(0)
    t = threading.Thread(target=worker, daemon=True)
    t.start()

def pid_alive(pid):
    """真·存活检查。

    绝不能用 os.kill(pid, 0) 当探针：CPython 在 Windows 上把非
    CTRL_C_EVENT/CTRL_BREAK_EVENT 的 sig 直接交给 TerminateProcess，
    于是 os.kill(pid, 0) == 立即 TerminateProcess(pid, 0) ——
    被"检查"的 QEMU 当场被杀，脚本后续的 screendump 全部落空，
    表象就是"驱动挂住、snapshot 是空的"。这里按平台各走各的路。"""
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
        STILL_ACTIVE = 259
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.OpenProcess.restype = wintypes.HANDLE
        h = k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not h:
            return False
        try:
            code = wintypes.DWORD()
            if not k32.GetExitCodeProcess(h, ctypes.byref(code)):
                return False
            return code.value == STILL_ACTIVE
        finally:
            k32.CloseHandle(h)
    try:
        os.kill(pid, 0)   # POSIX: 真正的探测语义（不发信号）
        return True
    except OSError:
        return False

def qmp_call(sock, rf, cmd, args=None):
    payload = json.dumps({"execute": cmd, "arguments": args or {}})
    sock.sendall(payload.encode() + b"\n")
    while True:
        line = rf.readline()
        if not line:
            raise ConnectionError("QMP connection closed")
        msg = json.loads(line)
        if msg.get("event"):
            continue
        if msg.get("error"):
            raise RuntimeError(f"QMP {cmd} error: {msg['error']}")
        return msg.get("return")

def sendkey(sock, rf, name, n=1):
    # name may be a chord "ctrl_r+c": a SINGLE send-key call with a key
    # array presses all keys down together, waits hold_time, then releases
    # them together - the guest sees a true chord (Ctrl held while 'c'
    # arrives). Sequential single-key calls would press+release ctrl_r
    # before 'c' arrives, losing the modifier (Task 7 closed loop).
    keys = [{"type": "qcode", "data": k} for k in name.split("+")]
    for i in range(n):
        qmp_call(sock, rf, "send-key", {"keys": keys, "hold-time": 80})
        if i < n - 1:
            time.sleep(0.3)

def rel_move(sock, rf, dx, dy):
    """一份 QMP rel 事件（X/Y 轴各自可选）。|DX|,|DY| <= HOVER_HID_MAX
    才不会被 QEMU 的 HID 报告钳位丢掉余量。"""
    evs = []
    if dx:
        evs.append({"type": "rel", "data": {"axis": "x", "value": int(dx)}})
    if dy:
        evs.append({"type": "rel", "data": {"axis": "y", "value": int(dy)}})
    if evs:
        qmp_call(sock, rf, "input-send-event", {"events": evs})

def click_btn(sock, rf, button="left"):
    """鼠标键沿（默认左键，press+release）。

    **不带坐标**：QMP input-send-event 的 btn 事件只管按键，指针停在
    hover|X|Y 刚标定的位置——这正是它比 VNC 指针事件可靠的原因。
    usb-mouse 是相对设备，QEMU 把 VNC 的绝对坐标换算成相对位移
    （ui/input.c 的 abs->rel），大跳变的位移会被 HID 启动协议报告的
    ±127 上限钳断，落点与请求值不符（theme_switch_probe.py 实测
    请求 (470,390) 落到 (645,407)）。故"定位"走 hover（分步走位，
    每步 <127），"点击"走本事件，两者解耦。
    用例写法：hover|X|Y t1 btn|left。"""
    btn_hold(sock, rf, button, True)
    time.sleep(0.06)
    btn_hold(sock, rf, button, False)

def btn_hold(sock, rf, button="left", down=True):
    """按下但不松开 / 松开（Token: down|left / up|left）。

    给"拖着画一笔"用：先 hover 定位，再 down|left，接着若干 rel|dx|dy，
    最后 up|left。

    **拖动过程必须用 rel 而不是 hover**：hover 每次都会先撞左上角复位，
    在按住状态下那一段大位移会被画成一条拉回原点的乱线。
    """
    qmp_call(sock, rf, "input-send-event",
             {"events": [{"type": "btn",
                          "data": {"down": bool(down), "button": button}}]})
    time.sleep(0.08)

def hover(sock, rf, x, y, screen_w, screen_h, abs_max):
    """把指针移到 guest 屏幕像素 (x, y)（Task B）。

    两步走：① 撞左上角——反复发大负向 rel，驱动侧的 [Min,Max] 钳位保证
    终态恒为设备 (0,0)，于是屏幕 (0,0)，与注入前的指针历史无关（开机初值
    是量程中点 512,512，不能假设）；② 从 (0,0) 按标定系数分步走到目标——
    每步 <=HOVER_STEP 且间隔 HOVER_SETTLE，使每步各成一份 HID 报告
    （并成一份会被钳到 ±127 而丢余量，这是"dead reckoning 不可靠"的根因）。

    用例侧只关心屏幕像素；rel↔像素的换算封在这里。"""
    for _ in range(HOVER_BANG_N):
        rel_move(sock, rf, -HOVER_HID_MAX, -HOVER_HID_MAX)
        time.sleep(HOVER_SETTLE)
    time.sleep(HOVER_EDGE_WAIT)
    dx = int(round(x * abs_max / screen_w))
    dy = int(round(y * abs_max / screen_h))
    while dx != 0 or dy != 0:
        sx = max(-HOVER_STEP, min(HOVER_STEP, dx))
        sy = max(-HOVER_STEP, min(HOVER_STEP, dy))
        rel_move(sock, rf, sx, sy)
        dx -= sx
        dy -= sy
        time.sleep(HOVER_SETTLE)
    print(f"[qmp] hover -> screen ({x},{y}) = device ({int(round(x*abs_max/screen_w))},"
          f"{int(round(y*abs_max/screen_h))})")

def _is_png(path):
    try:
        with open(path, "rb") as f:
            return f.read(8) == b"\x89PNG\r\n\x1a\n"
    except OSError:
        return False

def shot(sock, rf, path):
    """截一帧到 path（PNG）。

    不能假设 QEMU 会按扩展名选格式：本机 QEMU 10.2.50 实测**无视扩展名**，
    对 `screendump foo.png` 仍然写出 P6 PPM（1280*800*3 + 15 字节头）。
    于是这里改为"要 .png 名字 → 嗅探魔数 → 不是 PNG 就用 Pillow 转"，
    对任何 QEMU 版本都成立。

    Pillow 是可选依赖：缺失时保留原始 PPM 并打一行 WARN，不让抓帧流程
    整个失败（本机 managed Python 3.13 就没有 Pillow；cmd 里的 `python`
    解析到 Python310，那边有）。"""
    qmp_call(sock, rf, "screendump", {"filename": path})
    # screendump 是异步的（QEMU >= 9.x）：QMP 应答先到、文件后落盘，先等一下。
    for _ in range(100):
        if os.path.exists(path):
            break
        time.sleep(0.1)
    if not os.path.exists(path):
        raise FileNotFoundError(f"screendump did not produce {path}")
    if _is_png(path):
        return
    if not _HAVE_PIL:
        print(f"[qmp] WARN: {os.path.basename(path)} is not PNG and Pillow is "
              f"unavailable; leaving it as-is (it is a PPM)")
        return
    # 转换容错（2026-09-13 P14 全表 flake 实证）：screendump 的存在性轮询
    # 可能早于写盘完成——PIL 对截断 PPM 抛 OSError。重试 3 次；仍失败则
    # 保留原始文件并打 stdout WARN（**不打 stderr**——子进程 stderr 在
    # EAP=Stop 的 Run 脚本里会冒充异常）。
    tmp = path + ".tmp.png"
    for attempt in range(3):
        try:
            # 写盘完成前 PIL 会读到截断的 PPM；重试覆盖这个窗口。
            Image.open(path).convert("RGB").save(tmp, "PNG")
            os.replace(tmp, path)
            return
        except Exception as e:  # noqa: BLE001
            if attempt == 2:
                print(f"[qmp] WARN: ppm->png failed ({e}); raw frame kept at {path}")
            else:
                time.sleep(0.4)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=4555)
    ap.add_argument("--shot-dir", default="snapshot")
    ap.add_argument("--interval", type=float, default=2.0)
    ap.add_argument("--max-shots", type=int, default=40)
    ap.add_argument("--script", default="")
    ap.add_argument("--qemu-pid", type=int, default=0)
    # 整段驱动的墙钟预算（秒）。0 = 不限。
    # 存在的意义：这个脚本现在由 Run 脚本**同步**调用（不再 start /b 脱离），
    # 所以任何"等不到期望状态"的分支都必须有界——否则一次挂起会吃掉整个
    # 构建-运行循环。默认给足一次完整开机 + 交互用例的时间。
    ap.add_argument("--deadline", type=float, default=180.0)
    # hover|X|Y 的换算输入（缺省 = 本机 QEMU/OVMF 实测值，见文件头 HOVER_* 段）
    ap.add_argument("--screen-w", type=int, default=1280)
    ap.add_argument("--screen-h", type=int, default=800)
    ap.add_argument("--abs-max", type=int, default=1024)
    a = ap.parse_args()
    os.makedirs(a.shot_dir, exist_ok=True)
    t_start = time.time()
    # 比主循环的 --deadline 多给 15s 收尾余量，然后无条件结束进程。
    hard_exit_after(a.deadline + 15 if a.deadline else 300)
    time.sleep(1)
    for _ in range(30):
        try:
            sock = socket.create_connection(("127.0.0.1", a.port), timeout=5)
            break
        except OSError:
            time.sleep(1)
    else:
        sys.exit("cannot connect to QMP")
    # create_connection leaves its timeout on the returned socket, and the 1s
    # value used here before was too tight: the very first readline (the QMP
    # greeting) died with "TimeoutError: timed out" while the guest was still
    # coming up, which aborted the whole capture and produced an empty snapshot
    # directory. Reads are now given a real budget; the overall bound is
    # --deadline plus the hard-exit guard, not this timeout.
    sock.settimeout(30)
    rf = sock.makefile("r", encoding="utf-8", newline="\n")
    greeting = json.loads(rf.readline())   # QMP greeting, discard once
    if "QMP" not in greeting:
        sys.exit("bad QMP greeting")
    qmp_call(sock, rf, "qmp_capabilities")
    shots = 0
    tokens = a.script.split() if a.script else []
    i = 0
    stamp = time.strftime("%Y%m%d_%H%M%S")
    while True:
        if a.deadline and (time.time() - t_start) > a.deadline:
            print(f"[qmp] deadline {a.deadline}s reached after {shots} shot(s); stopping")
            break
        if tokens and i < len(tokens):
            t = tokens[i]; i += 1
            if t.startswith("t"):
                try:
                    time.sleep(float(t[1:])); continue
                except ValueError:
                    pass
            if t == "screendump":
                name = tokens[i]; i += 1
                p = os.path.join(a.shot_dir, f"{stamp}_{name}.png")
                shot(sock, rf, p); shots += 1; continue
            if t == "vmstop":
                qmp_call(sock, rf, "stop"); continue
            if t == "vmcont":
                qmp_call(sock, rf, "cont"); continue
            if t.startswith("rel|"):
                # 相对移动（QMP input-send-event 的 rel 轴——相对鼠标的唯一
                # 直通通道；VNC 的绝对坐标对相对设备不产生位移）。
                parts = t.split("|")
                if len(parts) >= 3:
                    rel_move(sock, rf, int(parts[1]), int(parts[2]))
                continue
            if t.startswith("hover|"):
                # 悬停到屏幕像素（Task B）：撞角复位 + 标定走位，全程 QMP。
                parts = t.split("|")
                if len(parts) >= 3:
                    hover(sock, rf, int(parts[1]), int(parts[2]),
                          a.screen_w, a.screen_h, a.abs_max)
                continue
            if t.startswith("btn|"):
                # 鼠标键沿（不移指针）：紧接 hover|X|Y 用 = 在标定点点击。
                # 与 vnc| 通道并存：需要"精确落点 + 点击"时用 hover+btn，
                # 需要滚轮/纯 VNC 注入时用 vnc|。
                parts = t.split("|")
                if len(parts) >= 2 and parts[1]:
                    click_btn(sock, rf, parts[1])
                continue
            if t.startswith("down|") or t.startswith("up|"):
                # 按下不松 / 松开。配合 rel| 使用即为"拖笔画线"：
                #   hover|X|Y t1 down|left t1 rel|80|40 ... t1 up|left
                parts = t.split("|")
                if len(parts) >= 2 and parts[1]:
                    btn_hold(sock, rf, parts[1], t.startswith("down|"))
                continue
            if t.startswith("vnc|"):
                # 内联 VNC 指针注入（滚轮验证）：阻塞直到 vnc_wheel 完成。
                parts = t.split("|")
                if len(parts) >= 4:
                    host, port = parts[1], int(parts[2])
                    cmds = parts[3].replace(",", " ")
                    vnc_py = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                          "vnc_wheel.py")
                    subprocess.run([sys.executable, vnc_py, "--host", host,
                                    "--port", str(port), "--script", cmds],
                                   check=False)
                continue
            # key with optional xN ("downx3" = 3 presses); a bare "x" token
            # (e.g. the letter x) must not crash the driver (int("") ValueError
            # or an empty qcode sent to QEMU)
            parts = t.split("x")
            name = parts[0] if parts[0] else t
            n = 1
            if len(parts) > 1 and parts[1].isdigit():
                n = int(parts[1])
            sendkey(sock, rf, name, n)
        else:
            time.sleep(a.interval)
            p = os.path.join(a.shot_dir, f"{stamp}_{(shots+1):03d}.png")
            shot(sock, rf, p); shots += 1
        if a.qemu_pid:
            if not pid_alive(a.qemu_pid):
                break
        if shots >= a.max_shots:
            break
    return 0

if __name__ == "__main__":
    sys.exit(main())
