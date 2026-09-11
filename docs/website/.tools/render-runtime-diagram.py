#!/usr/bin/env python3
"""Export homepage architecture artwork as SVG sources and 2x PNG images.

Requires CairoSVG and Noto Sans CJK SC. Run from any directory:
    python3 docs/website/.tools/render-runtime-diagram.py
"""

from pathlib import Path
from xml.sax.saxutils import escape

import cairosvg


OUT = Path(__file__).resolve().parents[1] / "assets"
INK = "#183457"
MUTED = "#536b87"
BLUE = "#285fa7"


def render(lang, narrow):
    zh = lang == "zh"
    width, height = (480, 670) if narrow else (1100, 600)
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img">',
        "<title>Holon Runtime · "
        + ("常驻后台与多端连接" if zh else "One service, multiple interfaces")
        + "</title>",
        '<defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="6" refY="3" '
        'orient="auto-start-reverse"><path d="M0 0L6 3L0 6" fill="none" '
        f'stroke="{BLUE}" stroke-width="1.3"/></marker></defs>',
        f'<rect width="{width}" height="{height}" rx="16" fill="#f7faff"/>',
        '<g font-family="Noto Sans CJK SC, sans-serif">',
    ]

    def rect(x, y, w, h, fill="#fff", stroke="#c4d3e6", dashed=False, radius=8):
        dash = ' stroke-dasharray="6 5"' if dashed else ""
        parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" '
            f'fill="{fill}" stroke="{stroke}" stroke-width="1.5"{dash}/>'
        )

    def text(x, y, value, size=18, color=INK, weight=400, anchor="start"):
        parts.append(
            f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" '
            f'font-weight="{weight}" text-anchor="{anchor}">{escape(value)}</text>'
        )

    def line(path, dashed=False, arrow=False):
        attrs = ' stroke-dasharray="6 5"' if dashed else ""
        if arrow:
            attrs += ' marker-end="url(#arrow)"'
        parts.append(
            f'<path d="{path}" fill="none" stroke="{BLUE}" stroke-width="1.6"{attrs}/>'
        )

    text(24 if narrow else 48, 32, "连接入口" if zh else "INTERFACES", 15, MUTED, 500)
    node_w, node_h, node_y = (128, 68, 57) if narrow else (190, 68, 54)
    starts = [24, 176, 328] if narrow else [185, 455, 725]
    for i, (name, subtitle) in enumerate([
        ("TUI", "终端" if zh else "Terminal"),
        ("Web UI", "浏览器" if zh else "Browser"),
        ("Mobile UI", "未来构想" if zh else "Future concept"),
    ]):
        x = starts[i]
        rect(x, node_y, node_w, node_h, fill="#f7faff" if i == 2 else "#fff",
             stroke="#8ca1b9" if i == 2 else "#99b5d6", dashed=i == 2)
        text(x + node_w / 2, node_y + 29, name, 19, MUTED if i == 2 else INK, 600, "middle")
        text(x + node_w / 2, node_y + 52, subtitle, 14, MUTED, anchor="middle")
        center = x + node_w / 2
        line(f"M{center} {node_y + node_h}V151", dashed=i == 2)

    # The future interface joins only through a dashed segment.
    centers = [x + node_w / 2 for x in starts]
    line(f"M{centers[0]} 151H{centers[1]}")
    line(f"M{centers[1]} 151H{centers[2]}", dashed=True)

    host_x, host_y, host_w, host_h = (16, 176, 448, 469) if narrow else (85, 178, 930, 392)
    rect(host_x, host_y, host_w, host_h, fill="#eef4fc", stroke="#d0dded", radius=12)
    text(host_x + 20, host_y + 29, "你的机器 / 服务器" if zh else "YOUR MACHINE / SERVER", 15, MUTED)

    rx, ry, rw, rh = (36, 236, 408, 287) if narrow else (120, 237, 860, 208)
    line(f"M{centers[1]} 151V{ry}", arrow=True)
    rect(rx, ry, rw, rh, fill="#fff", stroke="#6992c3", radius=10)
    text(rx + 23, ry + 40, "Holon Runtime", 30 if narrow else 32, INK, 700)
    text(rx + 23, ry + 68, "常驻后台 · daemon 模式" if zh else "Background service · daemon mode", 16, MUTED)

    modules = [
        ("Agents", "长期身份与职责" if zh else "Identity & responsibilities"),
        ("WorkItems", "目标、进度与结果" if zh else "Goals, progress & results"),
        ("Wait / Wake", "等待条件与工作续接" if zh else "Wait conditions & resumption"),
    ]
    for i, (name, detail) in enumerate(modules):
        if narrow:
            mx, my, mw, mh = rx + 18, ry + 91 + i * 59, rw - 36, 51
            rect(mx, my, mw, mh, fill="#f2f6fc", stroke="#dce6f3", radius=5)
            text(mx + 12, my + 22, name, 18, BLUE, 600)
            text(mx + 12, my + 42, detail, 14, MUTED)
        else:
            mx, my, mw, mh = rx + 23 + i * 275, ry + 100, 263, 79
            rect(mx, my, mw, mh, fill="#f2f6fc", stroke="#dce6f3", radius=5)
            text(mx + 16, my + 30, name, 23, BLUE, 600)
            text(mx + 16, my + 57, detail, 15, MUTED)

    wy = 569 if narrow else 487
    line(f"M{width / 2} {ry + rh}V{wy}", arrow=True)
    rect(rx, wy, rw, 56, fill="#f7faff", stroke="#c4d3e6")
    text(width / 2, wy + 23, "工作区 · 文件 · 工具链" if zh else "Workspaces · files · toolchains",
         18, INK, 500, "middle")
    text(width / 2, wy + 44, "在明确授权范围内执行" if zh else "Execution within explicit authorization",
         13, MUTED, anchor="middle")
    parts.append("</g></svg>")
    svg = "\n".join(parts) + "\n"
    stem = f"runtime-architecture-{lang}{'-narrow' if narrow else ''}"
    (OUT / f"{stem}.svg").write_text(svg)
    cairosvg.svg2png(bytestring=svg.encode(), write_to=str(OUT / f"{stem}.png"), scale=2)
    print(f"{stem}: {width}×{height} SVG, {width * 2}×{height * 2} PNG")


if __name__ == "__main__":
    for language in ("zh", "en"):
        for mobile in (False, True):
            render(language, mobile)
