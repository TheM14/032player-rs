#!/usr/bin/env python3
"""Converts the redesign icon set (Lucide style, 24 grid) to Slint path commands.

    python3 tools/icons/svg2slint.py prototypes/_src/icons.svg

Prints `name: "commands"` per symbol. Circles and rounded rects become arcs,
relative commands become absolute ones (one path string per icon, as the
`Icon` component in `crates/app/ui/controls.slint` expects).
"""
import re
import sys
import xml.etree.ElementTree as ET

NUM = r"[-+]?(?:\d+\.\d*|\.\d+|\d+)(?:[eE][-+]?\d+)?"


def fmt(v):
    v = round(v, 3)
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def parse(d):
    """Absolute commands for a path `d`."""
    # Split packed arc flags: after 3 numbers of an arc, flags are 0/1 digits.
    raw = re.findall(rf"[MmLlHhVvCcSsQqTtAaZz]|{NUM}", d)
    out = []
    x = y = sx = sy = 0.0
    i = 0
    cmd = None
    counts = dict(M=2, L=2, H=1, V=1, C=6, S=4, Q=4, T=2, A=7, Z=0)

    def take(n):
        nonlocal i
        vals = []
        while len(vals) < n:
            t = raw[i]
            i += 1
            vals.append(t)
        return vals

    while i < len(raw):
        t = raw[i]
        if re.fullmatch(r"[A-Za-z]", t):
            cmd = t
            i += 1
            if cmd in "Zz":
                out.append("Z")
                x, y = sx, sy
                continue
        up = cmd.upper()
        rel = cmd.islower()
        if up == "A":
            # rx ry rot then flags (maybe packed) then x y
            rx, ry, rot = (float(v) for v in take(3))
            flags = []
            while len(flags) < 2:
                tok = raw[i]
                i += 1
                if len(tok) > 1 and all(ch in "01" for ch in tok.replace(".", "")) and "." not in tok:
                    flags.extend(tok)
                else:
                    flags.append(tok)
            # A packed flag token may swallow the next number ("01.5"): rare
            # in this set, not handled.
            ex, ey = (float(v) for v in take(2))
            if rel:
                ex += x
                ey += y
            out.append(f"A {fmt(rx)} {fmt(ry)} {fmt(rot)} {int(float(flags[0]))} {int(float(flags[1]))} {fmt(ex)} {fmt(ey)}")
            x, y = ex, ey
            continue
        vals = [float(v) for v in take(counts[up])]
        if up == "M":
            if rel:
                vals = [vals[0] + x, vals[1] + y]
            x, y = vals
            sx, sy = x, y
            out.append(f"M {fmt(x)} {fmt(y)}")
            cmd = "l" if rel else "L"
        elif up == "L" or up == "T":
            if rel:
                vals = [vals[0] + x, vals[1] + y]
            x, y = vals
            out.append(f"{up} {fmt(x)} {fmt(y)}")
        elif up == "H":
            x = vals[0] + (x if rel else 0)
            out.append(f"L {fmt(x)} {fmt(y)}")
        elif up == "V":
            y = vals[0] + (y if rel else 0)
            out.append(f"L {fmt(x)} {fmt(y)}")
        elif up in "CSQ":
            pts = []
            for k in range(0, len(vals), 2):
                px, py = vals[k], vals[k + 1]
                if rel:
                    px += x
                    py += y
                pts.append((px, py))
            x, y = pts[-1]
            out.append(up + " " + " ".join(f"{fmt(a)} {fmt(b)}" for a, b in pts))
    return out


def circle(cx, cy, r):
    return [f"M {fmt(cx - r)} {fmt(cy)}", f"A {fmt(r)} {fmt(r)} 0 1 0 {fmt(cx + r)} {fmt(cy)}",
            f"A {fmt(r)} {fmt(r)} 0 1 0 {fmt(cx - r)} {fmt(cy)}", "Z"]


def rect(x, y, w, h, rx):
    if rx <= 0:
        return [f"M {fmt(x)} {fmt(y)}", f"L {fmt(x + w)} {fmt(y)}", f"L {fmt(x + w)} {fmt(y + h)}", f"L {fmt(x)} {fmt(y + h)}", "Z"]
    r = min(rx, w / 2, h / 2)
    return [
        f"M {fmt(x + r)} {fmt(y)}", f"L {fmt(x + w - r)} {fmt(y)}", f"A {fmt(r)} {fmt(r)} 0 0 1 {fmt(x + w)} {fmt(y + r)}",
        f"L {fmt(x + w)} {fmt(y + h - r)}", f"A {fmt(r)} {fmt(r)} 0 0 1 {fmt(x + w - r)} {fmt(y + h)}",
        f"L {fmt(x + r)} {fmt(y + h)}", f"A {fmt(r)} {fmt(r)} 0 0 1 {fmt(x)} {fmt(y + h - r)}",
        f"L {fmt(x)} {fmt(y + r)}", f"A {fmt(r)} {fmt(r)} 0 0 1 {fmt(x + r)} {fmt(y)}", "Z",
    ]


def main():
    tree = ET.parse(sys.argv[1])
    ns = "{http://www.w3.org/2000/svg}"
    for sym in tree.getroot().iter(ns + "symbol"):
        parts = []
        for el in sym:
            tag = el.tag.replace(ns, "")
            a = {k: el.get(k) for k in el.keys()}
            if tag == "path":
                parts += parse(a["d"])
            elif tag == "circle":
                parts += circle(float(a["cx"]), float(a["cy"]), float(a["r"]))
            elif tag == "rect":
                parts += rect(float(a["x"]), float(a["y"]), float(a["width"]), float(a["height"]), float(a.get("rx", 0)))
        print(f'{sym.get("id")}: "{" ".join(parts)}"')


if __name__ == "__main__":
    main()
