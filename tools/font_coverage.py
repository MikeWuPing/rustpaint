"""Parse an LVGL `lv_font_fmt_txt` .c file and report its exact codepoint coverage.

WHY THIS EXISTS
---------------
LVGL's built-in CJK fonts are *subsets*: a character that was not in the
converter's range list gets no glyph, and LVGL then renders **nothing at all**
for it -- no box, no placeholder. A Chinese UI built on guesses therefore ends
up with silently blank labels. This tool answers "is character X really in this
font?" before you ship the UI text.

The cmap encodings are easy to get wrong, and this file exists partly because
they were got wrong once already:

  * `unicode_list` in a SPARSE cmap holds **offsets relative to `range_start`**,
    not absolute codepoints. LVGL's own lookup does
        uint32_t rcp = letter - cmap->range_start;
        ... bsearch(rcp, unicode_list, list_length) ...
    (lv_font_fmt_txt.c, get_glyph_dsc_id). So codepoint = range_start + entry.
    Reading the entries as absolute yields a plausible-looking but completely
    wrong coverage set -- which is exactly the bug this doc is warning about.
  * `FORMAT0_TINY` covers the whole contiguous range.
  * `FORMAT0_FULL` also covers a contiguous range but maps through
    `glyph_id_ofs_list`; an offset of 0 means "no glyph", so the range has holes.
  * `SPARSE_FULL` uses relative offsets too, plus a glyph-id offset list.

Usage:
    python tools/font_coverage.py <font.c> ["string" ...]

Exit code 0 when every character of every given string is covered (or when no
strings were given); 1 when something is missing.
"""
import re
import sys


def parse_arrays(text):
    """name -> list[int] for every `static const <type> name[] = {...};`."""
    out = {}
    for m in re.finditer(r'static const \w+ (\w+)\[\]\s*=\s*\{(.*?)\};', text, re.S):
        out[m.group(1)] = [int(x, 0) for x in
                           re.findall(r'0x[0-9a-fA-F]+|\d+', m.group(2))]
    return out


def coverage(path):
    """Return the set of Unicode codepoints this font can actually render."""
    text = open(path, encoding='utf-8', errors='replace').read()
    block = re.search(r'cmaps\[\]\s*=\s*\{(.*?)\n\};', text, re.S)
    if not block:
        sys.exit('not an lv_font_fmt_txt .c file (no cmaps[]): ' + path)
    arrays = parse_arrays(text)
    covered = set()

    for entry in re.findall(r'\{\s*(.*?)\s*\}', block.group(1), re.S):
        d = dict(re.findall(r'\.(\w+)\s*=\s*([A-Za-z0-9_]+)', entry))
        if 'range_start' not in d:
            continue
        start = int(d['range_start'])
        length = int(d['range_length'])
        kind = d.get('type', '')
        ofs_list = arrays.get(d.get('glyph_id_ofs_list') or '', None)

        if 'SPARSE' in kind:
            # unicode_list holds offsets RELATIVE to range_start (see LVGL's
            # get_glyph_dsc_id). Offsets are sorted, so bsearch works there.
            for off in arrays.get(d.get('unicode_list') or '', []):
                covered.add(start + off)
        else:
            for rcp in range(length):
                if ofs_list is not None and ofs_list[rcp] == 0:
                    continue   # glyph_id_ofs_list == 0 -> notdef, no glyph
                covered.add(start + rcp)
    return covered


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    font_path = sys.argv[1]
    covered = coverage(font_path)
    cjk = sorted(c for c in covered if 0x4E00 <= c <= 0x9FFF)
    print(font_path)
    print(f"  covered codepoints : {len(covered)}")
    if covered:
        print(f"  range              : U+{min(covered):04X} .. U+{max(covered):04X}")
    print(f"  ASCII 0x20-0x7E    : {'complete' if all(c in covered for c in range(0x20, 0x7F)) else 'INCOMPLETE'}")
    print(f"  CJK  U+4E00-9FFF   : {len(cjk)}")
    if cjk:
        print(f"  CJK sample         : {''.join(chr(c) for c in cjk[:48])}")
    syms = sorted(c for c in covered if c >= 0xF000)
    print(f"  symbol glyphs      : {len(syms)}")

    if len(sys.argv) > 2:
        print("\n  string check:")
        total_missing = 0
        for s in sys.argv[2:]:
            missing = [ch for ch in s if ord(ch) not in covered]
            total_missing += len(missing)
            if missing:
                detail = "  missing: " + " ".join(f"{ch}(U+{ord(ch):04X})" for ch in missing)
            else:
                detail = ""
            print(f"    {'OK  ' if not missing else 'MISS'} {s}{detail}")
        return 1 if total_missing else 0
    return 0


if __name__ == '__main__':
    sys.exit(main())
