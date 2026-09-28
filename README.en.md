# rustupaint

**A little Paint for the UEFI Shell — application logic in Rust, rendering by LVGL 9.2.2.**

> **[简体中文](README.md) | English**

![License](https://img.shields.io/badge/license-MIT-green)
![Language](https://img.shields.io/badge/language-Rust%20%2B%20C-orange)
![Platform](https://img.shields.io/badge/platform-UEFI%20Shell%20(x86__64)-blue)
![Version](https://img.shields.io/badge/version-v0.1.0.24-blueviolet)
![LVGL](https://img.shields.io/badge/LVGL-9.2.2-22b8cf)

Before any operating system exists, the screen usually shows nothing but a `Shell>`
prompt. rustupaint replaces that black rectangle with a Windows 11-flavoured drawing
surface: a menu bar, a tool strip, a palette, a status bar, and a canvas you can draw
on with the mouse.

This is not an emulator. It is a real `.efi` that runs on the firmware, reads the GOP
framebuffer, and consumes USB mouse events.

---

## 1. What it looks like

### Eight tools, hover for a tooltip

Park the pointer over an icon on the left for about 0.4 s and a tooltip with the tool
name and a one-line hint fades in. LVGL 9 has no native hover state, so this is done by
polling the pointer position every tick and doing the hit test by hand.

![Tool tooltips](docs/manual/images/tools.gif)

### Full walkthrough

Pick a colour → pencil → line → rectangle → ellipse → flood fill → picker → menu →
undo → about. Every frame below is a real QEMU screendump, not a mockup.

![Full demo](docs/manual/images/demo.gif)

### The interface

![Overview](docs/manual/images/01-overview.png)

| Region | Description |
|---|---|
| Title bar | Application name centred; author credit pinned to the top-right |
| Menu bar | File / Edit / Help, Windows 11-style dropdown cards |
| Tool strip | 8 hand-drawn icons with Chinese tooltips on hover |
| Canvas | Centred white document; drag the mouse to draw |
| Palette | 16 swatches in a single row; the leftmost block previews the current colour |
| Status bar | Current tool, colour, cursor coordinates, canvas size, version |

---

## 2. Features

| Area | State |
|---|---|
| UI language | Chinese. Requires a CJK font — labels go through `FONT_CJK` / `FONT_CJK_SMALL` and vanish silently on a Latin-only face. `python tools/gen_cjk_font.py --check` verifies coverage against the source strings |
| Tools | Pencil, Eraser, Line, Rectangle, Ellipse, Fill, Colour picker, Clear |
| Icons | Eight 26×26 hand-drawn icons rasterised onto ARGB8888 sub-canvases (`rust/src/icon.rs`), so they stay correct over every button state (normal / pressed / focused) |
| Palette | 16 swatches in one row, live preview, picker retrieves any colour from the canvas |
| Undo | Bounded 8-level snapshot stack |
| Keyboard | `Tab` / `Shift+Tab` move focus — the focused control gets a 2px accent ring and a highlight background while unfocused controls stay flat (the Win11 "focused vs low-light" look); `Enter` activates, `Esc` closes menus and dialogs, arrow keys navigate inside popups |
| Mouse | Click tools and swatches, drag on the canvas to draw; strokes are interpolated so fast drags stay connected |
| Menus | File (New, Exit), Edit (Undo, Clear), Help (About) |
| Dialogs | Win11-style modal card on a scrim, with the focus group scoped to the dialog while it is open |

**The fully illustrated manual lives at [`docs/manual/index.html`](docs/manual/index.html)**
— 12 chapters, 29 real screenshots, 2 GIFs.

---

## 3. Architecture: EDK2 links, Rust thinks

The interesting part is not that it draws on UEFI. It is how the pieces are joined.

```
        Rust  (rust/src, ~2500 lines)
   app.rs      layout, event dispatch, menus, dialogs, drawing, hover tooltips
   canvas.rs   document model + raster primitives (pencil, line, rect, ellipse,
               flood fill, bounded undo) — pure pixels, no UI concepts
   icon.rs     the 8 tool-strip icons, drawn as raster ops onto ARGB sub-canvases
   theme.rs    Win11 colour tokens, metrics, tool names (Chinese)
   ffi.rs      the only place in the project that names C symbols
   widget.rs   Win11-styled control primitives built on the shim
        |
        |  flat C ABI, colours as 0xRRGGBB, one event trampoline
        v
        C   (RustPaintPkg/Application/RustPaint)
   UefiMain.c  entry: version assertion, port lifecycle, main-loop pacing
   RpShim.c    translates LVGL types and enums into the stable RP_* ABI
        |
        v
      LVGL 9.2.2  (LvglPkg: LvglLib + LvglUefiPort)
        |
        v
   GOP framebuffer / SimpleTextIn keyboard / USB mouse
```

Rust is compiled by cargo into a **static library**, and **EDK2 is the top-level linker**:
`RustPaint.inf` hands `rp_core.lib` to the link line as an *input file*
(`/LIBPATH:$(MODULE_DIR) rp_core.lib`). That keeps every workspace convention intact
(DSC/INF packages, the LvglPkg linkage, the serial version-assertion channel) while
keeping the LVGL and EDK2 headers out of the cargo build — **no bindgen needed**.

Two details of that integration are load-bearing, and both cost a debugging session:

- **`/DEFAULTLIB:` does not work here** — EDK2's link command starts with
  `/NODEFAULTLIB`, which ignores every `/DEFAULTLIB:` directive. The library must be
  named on the command line as an input file.
- **The library is `rp_core.lib`, not `rustupaint.lib`** — EDK2 names the module's own
  object archive `<BASE_NAME>.lib`, so a same-named library resolves to that archive
  instead of ours (`rustupaint.lib(UefiMain.obj) : error LNK2001: unresolved rp_app_build`).

### Three rules at the C ABI boundary

`RpShim.h` is the whole contract, and it holds three rules:

1. **No LVGL types or enums may appear in it.** Only `UINT32` / `UINT64` scalars are
   allowed; colours cross as `0x00RRGGBB` plus a separate opacity byte. Rust therefore
   never includes an EDK2 header.
2. **Rust does not know what `lv_obj_t`, `lv_color_t`, `LV_ALIGN_*` or `LV_EVENT_*` are.**
   Event codes, keys, alignments, fonts and flags are all translated in C into stable
   `RP_*` values. Change one side and you must change `rust/src/ffi.rs`'s constant of
   the same name.
3. **One event trampoline.** Objects carry a packed `user` value (class in the high byte,
   index in the low 24 bits); a single `extern "C"` function dispatches. The entire UI
   event path performs **zero dynamic allocation** — no closures, no `Box<dyn Fn>`,
   nothing for the no_std allocator to trip over.

---

## 4. Repository layout

```
RustInUEFI/
├── rust/                    Rust side (cargo produces rp_core.lib)
│   ├── src/                 app / canvas / icon / theme / ffi / widget
│   └── Cargo.toml           crate-type = ["staticlib"]
├── RustPaintPkg/            EDK2 package: C shim + application entry
│   └── Application/RustPaint/
│       ├── RpShim.h/.c      the C ABI contract and its implementation
│       └── UefiMain.c       entry point and main loop
├── LvglPkg/                 LVGL 9.2.2 + UEFI port layer (**not shipped — bring your own**)
├── tools/                   build and verification scripts
│   ├── Build-RustPaint.ps1  full build (cargo -> EDK2 -> dist/ + qemu_disk/)
│   ├── Run-RustPaintQemu.ps1  launch QEMU (interactive or scripted)
│   ├── qmp_drive.py         inject pointer/keys and capture frames over QMP
│   ├── check_stroke.py      pixel-level hit test (guards against coordinate drift)
│   ├── make_gif.py          stitch screenshots into a GIF
│   └── gen_cjk_font.py      CJK font generation and coverage check
├── docs/
│   ├── manual/index.html    product manual (12 chapters, 29 images, 2 GIFs)
│   └── 可行性调研.md         the feasibility study made before starting (Chinese)
├── CLAUDE.md                developer notes: traps and disciplines (worth reading)
└── req.md                   original requirements (Chinese)
```

---

## 5. Getting started

### Just want to run it

Grab `rustupaint.efi` from
[Releases](https://github.com/MikeWuPing/RustPaintUEFI/releases) (the bundle also
contains `OVMF_CODE.fd` and `startup.nsh`):

1. On real hardware: copy the files onto a FAT32 USB stick and run
   `fs0:\rustupaint.efi` from the UEFI Shell. The bundle also ships
   `EFI/BOOT/BOOTX64.EFI`, so the firmware can boot straight off the stick.
2. On a VM: unzip the bundle and run one command — QEMU's vvfat turns the folder
   into a boot disk, and OVMF boots `EFI/BOOT/BOOTX64.EFI` directly, so **no UEFI
   Shell and no disk image are needed**:

   ```powershell
   qemu-system-x86_64.exe -m 512 -vga std -net none -display sdl -usb -device usb-mouse `
     -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.fd `
     -drive format=raw,file=fat:rw:<unzipped folder>
   ```

   The bundle also ships `Run-Qemu.ps1` if you prefer to just launch that.

### Running under QEMU

```powershell
# Interactive: a real window with a working mouse — just play with it
powershell -ExecutionPolicy Bypass -File tools/Run-RustPaintQemu.ps1 -Interactive

# Headless: boot, capture framebuffer screenshots, inject pointer/keys, assert version
powershell -ExecutionPolicy Bypass -File tools/Run-RustPaintQemu.ps1 `
  -Script "t4 screendump main t1 hover|120|300 t1 btn|left t1 screendump drew"
```

Once you click into the window, QEMU captures the mouse — press **Left Ctrl + Left Alt**
to release it.

Evidence lands in `run_logs/` (serial + QEMU stderr) and `snapshot/` (PNG frames).
A run only counts as successful when the serial `APP_VERSION=` line matches
`expected_version.txt` **byte for byte** — the gate that stops a stale `.efi` from being
mistaken for a working build.

### Building from source

Requirements: EDK2 with the VS2019 toolchain, QEMU, a Rust toolchain
(`rustup` + the `x86_64-unknown-uefi` target), and **LvglPkg**.

> **LvglPkg is not shipped with this repository.** Its upstream,
> `MikeWuPing/UEFI_Tools`, is currently private — it can neither be referenced as a
> submodule nor redistributed publicly. Before building, place `LvglPkg/` at the
> repository root (next to `RustPaintPkg`), containing `LvglLib` (LVGL 9.2.2 itself)
> and `LvglUefiPort` (the UEFI port layer) — or assemble your own from the
> [official LVGL 9.2.2 sources](https://github.com/lvgl/lvgl) plus your own port.
> The repository's `.gitignore` already excludes `/LvglPkg/`, so a local copy will
> never be committed by accident.
>
> If you only want to see it run, skip all of this and download `rustupaint.efi`
> from Releases.

```powershell
rustup target add x86_64-unknown-uefi

# Full build (cargo static lib -> EDK2 link -> dist\ + qemu_disk\)
powershell -ExecutionPolicy Bypass -File tools/Build-RustPaint.ps1 -Target RELEASE

# No Rust toolchain yet? Validate everything except the Rust side with a C stub.
powershell -ExecutionPolicy Bypass -File tools/Build-RustPaint.ps1 -StubRust
```

> Windows note: the rustup shim under `%USERPROFILE%\.cargo\bin` can hang in some
> environments (`rustc --version` prints nothing and never returns). The build script
> therefore calls
> `%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\cargo.exe`
> directly.

---

## 6. Boundaries and known limitations

| Item | Note |
|---|---|
| Firmware | QEMU is launched **without** `-machine q35` and **without** a vars pflash, so OVMF falls back to its built-in UEFI Shell, which runs `fs0:\startup.nsh` and starts the app. Mouse input needs `-usb -device usb-mouse` |
| Resolution | Layout adapts to the current GOP resolution (1280×800 as measured under QEMU) |
| Undo | Fixed 8-level stack of full snapshots; deeper history costs proportionally more memory |
| Not implemented | File save / open (needs UEFI Simple File System plus a text input control); title-bar minimise / maximise / close buttons; flood-fill performance on large areas (currently a per-pixel scanline, which stalls on big regions under QEMU) |
| Tab key | The LVGL UEFI port maps `Tab` to a custom key rather than `LV_KEY_NEXT`, so focus navigation is driven by the application, not by LVGL's group machinery |

---

## 7. Licence

This project is MIT — see [`LICENSE`](LICENSE).

`LvglPkg/` is **not part of this repository** (see "Building from source"). It is a
separate dependency whose upstream is the private `MikeWuPing/UEFI_Tools` repository;
the LVGL core inside it follows LVGL's own MIT licence.

---

## 8. Author

**Mike Wu** · mikewuping@163.com · [GitHub](https://github.com/MikeWuPing)

The credit line appears both in the top-right of the title bar and in the About dialog;
both read the same `AUTHOR_LINE` / `AUTHOR_MAIL` constants in `app.rs`.

---

<p align="center">
  <a href="README.md">中文文档</a>
</p>
