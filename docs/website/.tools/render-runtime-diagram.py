#!/usr/bin/env python3
"""Export shared architecture artwork as SVG sources and 2x PNG images.

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
    width, height = (480, 550) if narrow else (1200, 350)
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

    def rect(x, y, w, h, fill="#fff", stroke="#c4d3e6", radius=6):
        parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" '
            f'fill="{fill}" stroke="{stroke}" stroke-width="1"/>'
        )

    def text(x, y, value, size=18, color=INK, weight=400, anchor="start"):
        # The article column can be much narrower than the viewport.
        if narrow:
            size = max(size, 20)
        parts.append(
            f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" '
            f'font-weight="{weight}" text-anchor="{anchor}">{escape(value)}</text>'
        )

    def line(path, arrow=False):
        attrs = ""
        if arrow:
            attrs += ' marker-end="url(#arrow)"'
        parts.append(
            f'<path d="{path}" fill="none" stroke="{BLUE}" stroke-width="1.2"{attrs}/>'
        )

    # Both layouts share labels and relationships; only their coordinates differ.
    text(32, 28 if narrow else 92, "操作入口" if zh else "INTERFACES", 16, MUTED, 500)
    for i, (name, subtitle) in enumerate([
        ("TUI", "终端" if zh else "Terminal"),
        ("Web UI", "浏览器" if zh else "Browser"),
    ]):
        x, y, w = (32 + i * 224, 46, 192) if narrow else (32, 116 + i * 80, 168)
        rect(x, y, w, 60)
        text(x + w / 2, y + 25, name, 20, INK, 500, "middle")
        text(x + w / 2, y + 47, subtitle, 16, MUTED, anchor="middle")

    host_x, host_y, host_w, host_h = (12, 152, 456, 386) if narrow else (300, 24, 880, 302)
    rect(host_x, host_y, host_w, host_h, fill="#f8fafc", stroke="#dce3ec")
    text(host_x + 20, host_y + 28, "你的机器 / 服务器" if zh else "HOST MACHINE", 16, MUTED)

    rx, ry, rw, rh = (32, 200, 416, 216) if narrow else (330, 78, 460, 222)
    if narrow:
        line("M128 106V128H352V106")
        line(f"M240 128V{ry}", arrow=True)
        text(252, 146, "连接" if zh else "Connect", 16, BLUE)
    else:
        line("M200 146H230V226H200")
        line(f"M230 186H{rx}", arrow=True)
        text(238, 174, "连接" if zh else "Connect", 14, BLUE)

    rect(rx, ry, rw, rh, stroke="#9ab1cd")
    text(rx + 22, ry + 34, "Holon Runtime", 25, INK, 500)
    text(rx + 22, ry + 60, "常驻后台 · daemon 模式" if zh else "Background service · daemon mode", 16, MUTED)

    modules = [
        ("Agents", "长期身份与职责" if zh else "Identity & roles"),
        ("WorkItems", "目标、进度与结果" if zh else "Goals, progress & results"),
        ("Wait / Wake", "等待条件与工作续接" if zh else "Wait & resume"),
    ]
    for i, (name, detail) in enumerate(modules):
        baseline = ry + 103 + i * 42
        text(rx + 22, baseline, name, 18, BLUE, 500)
        text(rx + 154, baseline, detail, 16, MUTED)

    if narrow:
        line("M240 416V472", arrow=True)
        text(252, 452, "执行" if zh else "Execute", 16, BLUE)
        text(240, 496, "工作区 · 文件 · 工具链" if zh else "Workspaces · files · toolchains",
             20, INK, 500, "middle")
        text(240, 522, "在明确授权范围内" if zh else "Within explicit authorization",
             16, MUTED, anchor="middle")
    else:
        line("M790 186H884", arrow=True)
        text(810, 174, "执行" if zh else "Execute", 16, BLUE)
        for i, label in enumerate(
            ["工作区", "文件与工具链"] if zh else ["Workspaces", "Files & toolchains"]
        ):
            text(908, 162 + i * 34, label, 22, INK, 500)
        for i, label in enumerate(
            ["在明确授权范围内执行"] if zh else ["Execution within", "explicit authorization"]
        ):
            text(908, 238 + i * 24, label, 16, MUTED)
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
