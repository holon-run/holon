#!/usr/bin/env python3
"""Export homepage architecture artwork as SVG sources and 2x PNG images.

Requires CairoSVG and Noto Sans CJK SC. Run from any directory:
    python3 docs/website/.tools/render-runtime-diagram.py
    python3 docs/website/.tools/render-runtime-diagram.py --language zh
"""

from pathlib import Path
import argparse
from xml.sax.saxutils import escape

import cairosvg


OUT = Path(__file__).resolve().parents[1] / "assets"
INK = "#183457"
MUTED = "#536b87"
BLUE = "#285fa7"


def render(lang, narrow):
    zh = lang == "zh"
    width, height = (480, 610) if narrow else (1100, 520)
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img">',
        "<title>Holon Runtime · "
        + ("常驻后台与多端连接" if zh else "One service, multiple interfaces")
        + "</title>",
        '<defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="6" refY="3" '
        'orient="auto-start-reverse"><path d="M0 0L6 3L0 6" fill="none" '
        f'stroke="{BLUE}" stroke-width="1.3"/></marker></defs>',
        f'<rect width="{width}" height="{height}" fill="#fff"/>',
        '<g font-family="Noto Sans CJK SC, sans-serif">',
    ]

    def rect(x, y, w, h, fill="#fff", stroke="#c4d3e6", dashed=False, radius=8):
        dash = ' stroke-dasharray="6 5"' if dashed else ""
        parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" '
            f'fill="{fill}" stroke="{stroke}" stroke-width="{1.3 if narrow else 1}"{dash}/>'
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
            f'<path d="{path}" fill="none" stroke="{BLUE}" stroke-width="1.2"{attrs}/>'
        )

    text(24 if narrow else 48, 32, "连接入口" if zh else "INTERFACES", 15, MUTED, 500)
    node_w, node_h, node_y = (128, 58, 51) if narrow else (190, 58, 48)
    starts = [24, 176, 328] if narrow else [185, 455, 725]
    for i, (name, subtitle) in enumerate([
        ("TUI", "终端" if zh else "Terminal"),
        ("Web UI", "浏览器" if zh else "Browser"),
        ("Mobile UI", "未来构想" if zh else "Future concept"),
    ]):
        x = starts[i]
        rect(x, node_y, node_w, node_h, stroke="#b8c6d6", dashed=i == 2, radius=5)
        text(x + node_w / 2, node_y + 24, name, 18, MUTED if i == 2 else INK, 500, "middle")
        text(x + node_w / 2, node_y + 46, subtitle, 16 if narrow else 14, MUTED, anchor="middle")
        center = x + node_w / 2
        line(f"M{center} {node_y + node_h}V133", dashed=i == 2)

    # The future interface joins only through a dashed segment.
    centers = [x + node_w / 2 for x in starts]
    line(f"M{centers[0]} 133H{centers[1]}")
    line(f"M{centers[1]} 133H{centers[2]}", dashed=True)

    host_x, host_y, host_w, host_h = (16, 154, 448, 438) if narrow else (85, 158, 930, 338)
    rect(host_x, host_y, host_w, host_h, fill="#f8fafc", stroke="#b8c6d6" if narrow else "#dce3ec", radius=6)
    text(host_x + 20, host_y + 29, "你的机器 / 服务器" if zh else "YOUR MACHINE / SERVER", 16 if narrow else 15, MUTED)

    rx, ry, rw, rh = (36, 210, 408, 248) if narrow else (120, 213, 860, 166)
    line(f"M{centers[1]} 133V{ry}", arrow=True)
    rect(rx, ry, rw, rh, fill="#fff", stroke="#9ab1cd", radius=5)
    text(rx + 22, ry + 34, "Holon Runtime", 24 if narrow else 26, INK, 500)
    text(rx + 22, ry + 60, "常驻后台 · daemon 模式" if zh else "Background service · daemon mode", 16, MUTED)

    modules = [
        ("Agents", "长期身份与职责" if zh else "Identity & responsibilities"),
        ("WorkItems", "目标、进度与结果" if zh else "Goals, progress & results"),
        ("Wait / Wake", "等待条件与工作续接" if zh else "Wait conditions & resumption"),
    ]
    for i, (name, detail) in enumerate(modules):
        if narrow:
            mx, my = rx + 22, ry + 77 + i * 54
            text(mx, my + 18, name, 18, BLUE, 500)
            text(mx, my + 40, detail, 17, MUTED)
        else:
            mx, my = rx + 23 + i * 275, ry + 82
            text(mx, my + 24, name, 20, BLUE, 500)
            text(mx, my + 51, detail, 16, MUTED)

    wy = 504 if narrow else 421
    line(f"M{width / 2} {ry + rh}V{wy}", arrow=True)
    text(width / 2, wy + 23, "工作区 · 文件 · 工具链" if zh else "Workspaces · files · toolchains",
         18, INK, 500, "middle")
    text(width / 2, wy + 44, "在明确授权范围内执行" if zh else "Execution within explicit authorization",
         16 if narrow else 14, MUTED, anchor="middle")
    parts.append("</g></svg>")
    svg = "\n".join(parts) + "\n"
    stem = f"runtime-architecture-{lang}{'-narrow' if narrow else ''}"
    (OUT / f"{stem}.svg").write_text(svg)
    cairosvg.svg2png(bytestring=svg.encode(), write_to=str(OUT / f"{stem}.png"), scale=2)
    print(f"{stem}: {width}×{height} SVG, {width * 2}×{height * 2} PNG")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--language", choices=("zh", "en", "all"), default="all")
    args = parser.parse_args()
    for language in (("zh", "en") if args.language == "all" else (args.language,)):
        for mobile in (False, True):
            render(language, mobile)
