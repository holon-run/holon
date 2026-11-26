#!/usr/bin/env python3
"""Export shared runtime architecture artwork as SVG sources and 2x PNG images.

Uses CairoSVG when installed, falling back to rsvg-convert. Run from any directory:
    python3 docs/website/.tools/render-runtime-diagram.py
    python3 docs/website/.tools/render-runtime-diagram.py --language zh
"""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import subprocess
from xml.sax.saxutils import escape

try:
    import cairosvg  # type: ignore
except ImportError:
    cairosvg = None


OUT = Path(__file__).resolve().parents[1] / "assets"

INK = "#10233f"
SUBINK = "#2b4263"
MUTED = "#5b6e86"
BLUE = "#1f63d5"
BLUE_DARK = "#164ba4"
BLUE_SOFT = "#eaf2ff"
GREEN = "#238551"
GREEN_SOFT = "#e7f6ee"
HOST_BG = "#f2f6fc"
HOST_STROKE = "#dde7f5"
CARD_STROKE = "#d7e3f3"
ROW_BG = "#f6f9fe"
FONT = (
    "PingFang SC, Hiragino Sans GB, Noto Sans CJK SC, Lantinghei SC, "
    "-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
)
MONO = "SFMono-Regular, SF Mono, Menlo, Consolas, monospace"


def export_png(svg: str, png_path: Path, scale: int = 2) -> None:
    if cairosvg is not None:
        cairosvg.svg2png(bytestring=svg.encode("utf-8"), write_to=str(png_path), scale=scale)
        return
    rsvg = shutil.which("rsvg-convert")
    if not rsvg:
        raise RuntimeError("Install cairosvg or rsvg-convert to render PNG diagrams.")
    subprocess.run(
        [rsvg, "-z", str(scale), "-o", str(png_path)],
        input=svg.encode("utf-8"),
        check=True,
    )


def icon_svg(kind: str, cx: float, cy: float) -> str:
    """Return a clean 30x30 soft icon badge centered around (cx, cy)."""
    bg = f'<rect x="{cx - 15}" y="{cy - 15}" width="30" height="30" rx="8" fill="{BLUE_SOFT}"/>'
    if kind == "web":
        glyph = (
            f'<rect x="{cx - 8.5}" y="{cy - 6.5}" width="17" height="13" rx="2.5" '
            f'fill="none" stroke="{BLUE}" stroke-width="1.5"/>'
            f'<line x1="{cx - 8.5}" y1="{cy - 2.5}" x2="{cx + 8.5}" y2="{cy - 2.5}" '
            f'stroke="{BLUE}" stroke-width="1.3"/>'
        )
    elif kind == "tui":
        glyph = (
            f'<rect x="{cx - 8.5}" y="{cy - 6.5}" width="17" height="13" rx="2.5" '
            f'fill="none" stroke="{BLUE}" stroke-width="1.5"/>'
            f'<path d="M{cx - 5} {cy - 1.5}L{cx - 2} {cy + 1}L{cx - 5} {cy + 3.5}" '
            f'fill="none" stroke="{BLUE}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>'
            f'<line x1="{cx}" y1="{cy + 3.5}" x2="{cx + 4.5}" y2="{cy + 3.5}" '
            f'stroke="{BLUE}" stroke-width="1.5" stroke-linecap="round"/>'
        )
    elif kind == "mobile":
        glyph = (
            f'<rect x="{cx - 5.5}" y="{cy - 8}" width="11" height="16" rx="2.5" '
            f'fill="none" stroke="{BLUE}" stroke-width="1.5"/>'
            f'<line x1="{cx - 2}" y1="{cy + 5.2}" x2="{cx + 2}" y2="{cy + 5.2}" '
            f'stroke="{BLUE}" stroke-width="1.4" stroke-linecap="round"/>'
        )
    elif kind == "holon":
        glyph = (
            f'<polygon points="{cx},{cy - 8.5} {cx + 7.4},{cy - 4.2} {cx + 7.4},{cy + 4.2} '
            f'{cx},{cy + 8.5} {cx - 7.4},{cy + 4.2} {cx - 7.4},{cy - 4.2}" '
            f'fill="none" stroke="{BLUE}" stroke-width="1.6" stroke-linejoin="round"/>'
            f'<circle cx="{cx}" cy="{cy}" r="2.6" fill="{BLUE}"/>'
        )
    else:  # workspace
        glyph = (
            f'<path d="M{cx - 8.5} {cy - 4.5}a2 2 0 0 1 2 -2h4l2 2.2h7a2 2 0 0 1 2 2v6.8'
            f'a2 2 0 0 1 -2 2h-13a2 2 0 0 1 -2 -2z" '
            f'fill="none" stroke="{BLUE}" stroke-width="1.5" stroke-linejoin="round"/>'
        )
    return bg + glyph


def render_wide(zh: bool) -> str:
    width, height = 1200, 350
    title = "Holon · 常驻后台与多端连接" if zh else "Holon · One runtime, multiple clients"

    clients = [
        ("web", "Web UI", "浏览器工作台" if zh else "Browser workbench"),
        ("tui", "TUI & CLI", "终端与命令行" if zh else "Terminal & CLI"),
        ("mobile", "Mobile", "iOS · Android" if zh else "iOS & Android"),
    ]

    modules = [
        ("Agents", "长期身份、技能与记忆" if zh else "Lasting roles, skills & memory"),
        ("WorkItems", "目标、计划与待办进度" if zh else "Goals, plans & saved progress"),
        ("Wait / Wake", "事件等待与原地唤醒" if zh else "Event wait & in-place resume"),
    ]

    ws_items = (
        ["独立项目仓库与 Git Worktree", "Shell、编译器与测试工具链"]
        if zh
        else ["Project repos & Git worktrees", "Local shell, builds & test suites"]
    )

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img">',
        f"<title>{escape(title)}</title>",
        "<defs>",
        '<filter id="cardShadow" x="-8%" y="-8%" width="116%" height="124%">'
        '<feDropShadow dx="0" dy="6" stdDeviation="10" flood-color="#10233f" flood-opacity="0.055"/>'
        "</filter>",
        f'<marker id="arrow" markerWidth="8" markerHeight="8" refX="6.5" refY="4" orient="auto">'
        f'<path d="M1 1.2L6.5 4L1 6.8" fill="none" stroke="{BLUE}" stroke-width="1.5" '
        'stroke-linecap="round" stroke-linejoin="round"/></marker>',
        "</defs>",
        f'<rect width="{width}" height="{height}" fill="#ffffff"/>',
        f'<g font-family="{FONT}">',
    ]

    # Section label: Interfaces (outside host machine)
    parts.append(
        f'<text x="26" y="40" font-size="12.5" font-weight="700" letter-spacing="0.8" '
        f'fill="{MUTED}">{escape("多端接入" if zh else "CLIENTS")}</text>'
    )

    # Three equal, solid client cards (y = 58, 144, 230; height = 64)
    card_x, card_w, card_h = 24, 188, 64
    centers_y = []
    for idx, (kind, name, sub) in enumerate(clients):
        cy_top = 58 + idx * 86
        cy_mid = cy_top + card_h / 2
        centers_y.append(cy_mid)
        parts.append(
            f'<rect x="{card_x}" y="{cy_top}" width="{card_w}" height="{card_h}" rx="14" '
            f'fill="#ffffff" stroke="{CARD_STROKE}" stroke-width="1" filter="url(#cardShadow)"/>'
        )
        parts.append(icon_svg(kind, card_x + 30, cy_mid))
        parts.append(
            f'<text x="{card_x + 54}" y="{cy_top + 28}" font-size="16" font-weight="700" '
            f'fill="{INK}">{escape(name)}</text>'
        )
        parts.append(
            f'<text x="{card_x + 54}" y="{cy_top + 48}" font-size="13" fill="{MUTED}">'
            f"{escape(sub)}</text>"
        )

    # Host Machine container (x=312..1176, y=18..332)
    hx, hy, hw, hh = 312, 18, 864, 314
    parts.append(
        f'<rect x="{hx}" y="{hy}" width="{hw}" height="{hh}" rx="22" '
        f'fill="{HOST_BG}" stroke="{HOST_STROKE}" stroke-width="1"/>'
    )
    parts.append(f'<circle cx="{hx + 24}" cy="{hy + 25}" r="4" fill="{BLUE}"/>')
    parts.append(
        f'<text x="{hx + 36}" y="{hy + 29}" font-size="12.5" font-weight="700" '
        f'letter-spacing="0.8" fill="{SUBINK}">'
        f'{escape("你的电脑或团队服务器" if zh else "YOUR COMPUTER OR TEAM SERVER")}</text>'
    )

    # Smooth solid connector bus from all 3 clients -> Holon runtime card (x=336, y_mid=176)
    bus_x = 232
    target_y = centers_y[1]  # 176
    # Top client curve
    parts.append(
        f'<path d="M{card_x + card_w} {centers_y[0]} H{bus_x - 10} '
        f'Q{bus_x} {centers_y[0]} {bus_x} {centers_y[0] + 10} V{target_y}" '
        f'fill="none" stroke="{BLUE}" stroke-width="1.6"/>'
    )
    # Bottom client curve (SOLID — Mobile is a first-class shipped client!)
    parts.append(
        f'<path d="M{card_x + card_w} {centers_y[2]} H{bus_x - 10} '
        f'Q{bus_x} {centers_y[2]} {bus_x} {centers_y[2] - 10} V{target_y}" '
        f'fill="none" stroke="{BLUE}" stroke-width="1.6"/>'
    )
    # Middle client straight through to Holon card (rx=336)
    parts.append(
        f'<path d="M{card_x + card_w} {target_y} H332" '
        f'fill="none" stroke="{BLUE}" stroke-width="1.6" marker-end="url(#arrow)"/>'
    )
    # Small Connect pill cleanly centered between bus_x (232) and hx (312)
    conn_label = "连接" if zh else "Connect"
    pill_w = 46 if zh else 62
    pill_cx = 272
    pill_x = pill_cx - pill_w / 2
    parts.append(
        f'<rect x="{pill_x}" y="{target_y - 28}" width="{pill_w}" height="21" rx="10.5" '
        f'fill="{BLUE_SOFT}"/>'
        f'<text x="{pill_cx}" y="{target_y - 13.5}" font-size="11.5" font-weight="700" '
        f'fill="{BLUE_DARK}" text-anchor="middle">{escape(conn_label)}</text>'
    )

    # Card A: Holon Runtime Card (x=336..780, y=58..310)
    rx, ry, rw, rh = 336, 58, 444, 252
    parts.append(
        f'<rect x="{rx}" y="{ry}" width="{rw}" height="{rh}" rx="16" '
        f'fill="#ffffff" stroke="{CARD_STROKE}" stroke-width="1" filter="url(#cardShadow)"/>'
    )
    parts.append(icon_svg("holon", rx + 33, ry + 36))
    parts.append(
        f'<text x="{rx + 58}" y="{ry + 33}" font-size="21" font-weight="700" fill="{INK}">Holon</text>'
    )
    parts.append(
        f'<text x="{rx + 58}" y="{ry + 53}" font-size="13" fill="{MUTED}">'
        f'{escape("持续工作 · 状态保存 · 事件唤醒" if zh else "Ongoing work · saved state · event wakeups")}</text>'
    )

    # Always-on status pill in top-right of Holon card
    badge_text = "常驻服务" if zh else "Always-on"
    bw = 74 if zh else 84
    bx = rx + rw - bw - 18
    by = ry + 20
    parts.append(
        f'<rect x="{bx}" y="{by}" width="{bw}" height="24" rx="12" fill="{GREEN_SOFT}"/>'
        f'<circle cx="{bx + 13}" cy="{by + 12}" r="3.5" fill="{GREEN}"/>'
        f'<text x="{bx + 22}" y="{by + 16}" font-size="11.5" font-weight="700" fill="{GREEN}">'
        f"{escape(badge_text)}</text>"
    )

    # Three tonal responsibility rows inside Holon card
    for i, (m_name, m_desc) in enumerate(modules):
        my = ry + 74 + i * 54
        parts.append(
            f'<rect x="{rx + 18}" y="{my}" width="{rw - 36}" height="44" rx="10" fill="{ROW_BG}"/>'
        )
        parts.append(
            f'<text x="{rx + 34}" y="{my + 27}" font-family="{MONO}" font-size="14" '
            f'font-weight="700" fill="{BLUE_DARK}">{escape(m_name)}</text>'
        )
        parts.append(
            f'<text x="{rx + 158}" y="{my + 27}" font-size="13.5" fill="{SUBINK}">'
            f"{escape(m_desc)}</text>"
        )

    # Connector between Holon card and Workspaces card (x=776..852)
    exec_y = 184
    parts.append(
        f'<path d="M{rx + rw} {exec_y} H848" fill="none" stroke="{BLUE}" '
        'stroke-width="1.6" marker-end="url(#arrow)"/>'
    )
    exec_label = "执行" if zh else "Execute"
    ew = 46 if zh else 62
    ex = 814 - ew / 2
    parts.append(
        f'<rect x="{ex}" y="{exec_y - 31}" width="{ew}" height="21" rx="10.5" fill="#ffffff" '
        f'stroke="{HOST_STROKE}" stroke-width="1"/>'
        f'<text x="814" y="{exec_y - 16.5}" font-size="11.5" font-weight="700" '
        f'fill="{BLUE_DARK}" text-anchor="middle">{escape(exec_label)}</text>'
    )

    # Card B: Workspaces & Toolchains Card (x=854..1152, y=58..310)
    wx, wy, ww, wh = 854, 58, 298, 252
    parts.append(
        f'<rect x="{wx}" y="{wy}" width="{ww}" height="{wh}" rx="16" '
        f'fill="#ffffff" stroke="{CARD_STROKE}" stroke-width="1" filter="url(#cardShadow)"/>'
    )
    parts.append(icon_svg("workspace", wx + 33, wy + 36))
    parts.append(
        f'<text x="{wx + 58}" y="{wy + 33}" font-size="18" font-weight="700" fill="{INK}">'
        f'{escape("工作区与工具链" if zh else "Workspaces")}</text>'
    )
    parts.append(
        f'<text x="{wx + 58}" y="{wy + 53}" font-size="13" fill="{MUTED}">'
        f'{escape("本地文件与真实执行环境" if zh else "Files & local toolchains")}</text>'
    )

    for i, item in enumerate(ws_items):
        iy = wy + 76 + i * 54
        parts.append(
            f'<rect x="{wx + 18}" y="{iy}" width="{ww - 36}" height="44" rx="10" fill="{ROW_BG}"/>'
            f'<circle cx="{wx + 34}" cy="{iy + 22}" r="3" fill="{BLUE}"/>'
            f'<text x="{wx + 46}" y="{iy + 27}" font-size="13.5" font-weight="500" fill="{SUBINK}">'
            f"{escape(item)}</text>"
        )

    # Explicit authorization footer note inside Workspaces card
    parts.append(
        f'<rect x="{wx + 18}" y="{wy + 188}" width="{ww - 36}" height="44" rx="10" '
        'fill="#f8fafd" stroke="#e5edf7" stroke-width="1"/>'
        f'<text x="{wx + ww / 2}" y="{wy + 215}" font-size="12.5" font-weight="600" '
        f'fill="{MUTED}" text-anchor="middle">'
        f'{escape("在明确授权范围内执行" if zh else "Runs within explicit authorization")}</text>'
    )

    parts.append("</g></svg>")
    return "\n".join(parts) + "\n"


def render_narrow(zh: bool) -> str:
    width, height = 480, 550
    title = "Holon · 常驻后台与多端连接" if zh else "Holon · One runtime, multiple clients"

    clients = [
        ("web", "Web UI", "浏览器" if zh else "Browser"),
        ("tui", "TUI & CLI", "终端" if zh else "Terminal"),
        ("mobile", "Mobile", "手机端" if zh else "Phone"),
    ]
    modules = [
        ("Agents", "长期身份、技能与记忆" if zh else "Roles, skills & memory"),
        ("WorkItems", "目标、计划与待办进度" if zh else "Goals, plans & progress"),
        ("Wait / Wake", "事件等待与原地唤醒" if zh else "Event wait & resume"),
    ]

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img">',
        f"<title>{escape(title)}</title>",
        "<defs>",
        '<filter id="cardShadowN" x="-8%" y="-8%" width="116%" height="124%">'
        '<feDropShadow dx="0" dy="5" stdDeviation="8" flood-color="#10233f" flood-opacity="0.055"/>'
        "</filter>",
        f'<marker id="arrowN" markerWidth="8" markerHeight="8" refX="6.5" refY="4" orient="auto">'
        f'<path d="M1 1.2L6.5 4L1 6.8" fill="none" stroke="{BLUE}" stroke-width="1.5" '
        'stroke-linecap="round" stroke-linejoin="round"/></marker>',
        "</defs>",
        f'<rect width="{width}" height="{height}" fill="#ffffff"/>',
        f'<g font-family="{FONT}">',
        f'<text x="20" y="24" font-size="12.5" font-weight="700" letter-spacing="0.8" '
        f'fill="{MUTED}">{escape("多端接入" if zh else "CLIENTS")}</text>',
    ]

    # Top row: 3 equal solid client cards (width 138 each, gap 13 -> 20 + 138*3 + 26 = 460)
    centers_x = []
    for i, (kind, name, sub) in enumerate(clients):
        cx = 20 + i * 151
        cw, ch = 138, 74
        cmid = cx + cw / 2
        centers_x.append(cmid)
        parts.append(
            f'<rect x="{cx}" y="36" width="{cw}" height="{ch}" rx="13" '
            f'fill="#ffffff" stroke="{CARD_STROKE}" stroke-width="1" filter="url(#cardShadowN)"/>'
        )
        parts.append(
            f'<text x="{cmid}" y="67" font-size="16" font-weight="700" fill="{INK}" '
            f'text-anchor="middle">{escape(name)}</text>'
        )
        parts.append(
            f'<text x="{cmid}" y="91" font-size="13" fill="{MUTED}" '
            f'text-anchor="middle">{escape(sub)}</text>'
        )

    # Host Machine container (x=14..466, y=156..538)
    parts.append(
        f'<rect x="14" y="156" width="452" height="382" rx="20" '
        f'fill="{HOST_BG}" stroke="{HOST_STROKE}" stroke-width="1"/>'
    )
    parts.append(f'<circle cx="34" cy="178" r="4" fill="{BLUE}"/>')
    parts.append(
        f'<text x="46" y="182" font-size="12" font-weight="700" letter-spacing="0.7" '
        f'fill="{SUBINK}">{escape("你的电脑或团队服务器" if zh else "YOUR COMPUTER OR TEAM SERVER")}</text>'
    )

    # Solid merge bus from all 3 top client cards -> Holon card (y=196)
    bus_y = 123
    mid_x = centers_x[1]  # 240
    parts.append(
        f'<path d="M{centers_x[0]} 110 V{bus_y - 6} Q{centers_x[0]} {bus_y} {centers_x[0] + 6} {bus_y} H{mid_x}" '
        f'fill="none" stroke="{BLUE}" stroke-width="1.6"/>'
    )
    parts.append(
        f'<path d="M{centers_x[2]} 110 V{bus_y - 6} Q{centers_x[2]} {bus_y} {centers_x[2] - 6} {bus_y} H{mid_x}" '
        f'fill="none" stroke="{BLUE}" stroke-width="1.6"/>'
    )
    parts.append(
        f'<path d="M{mid_x} 110 V192" fill="none" stroke="{BLUE}" stroke-width="1.6" '
        'marker-end="url(#arrowN)"/>'
    )
    conn_label = "连接" if zh else "Connect"
    pw = 46 if zh else 64
    parts.append(
        f'<rect x="{mid_x + 10}" y="129" width="{pw}" height="20" rx="10" fill="{BLUE_SOFT}"/>'
        f'<text x="{mid_x + 10 + pw / 2}" y="143" font-size="11.5" font-weight="700" '
        f'fill="{BLUE_DARK}" text-anchor="middle">{escape(conn_label)}</text>'
    )

    # Holon card (x=30..450, y=196..408)
    rx, ry, rw, rh = 30, 196, 420, 212
    parts.append(
        f'<rect x="{rx}" y="{ry}" width="{rw}" height="{rh}" rx="15" '
        f'fill="#ffffff" stroke="{CARD_STROKE}" stroke-width="1" filter="url(#cardShadowN)"/>'
    )
    parts.append(icon_svg("holon", rx + 30, ry + 32))
    parts.append(
        f'<text x="{rx + 54}" y="{ry + 29}" font-size="20" font-weight="700" fill="{INK}">Holon</text>'
    )
    parts.append(
        f'<text x="{rx + 54}" y="{ry + 48}" font-size="12.5" fill="{MUTED}">'
        f'{escape("持续工作 · 状态保存 · 事件唤醒" if zh else "Ongoing work · saved state · event wakeups")}</text>'
    )
    for i, (m_name, m_desc) in enumerate(modules):
        my = ry + 64 + i * 46
        parts.append(
            f'<rect x="{rx + 16}" y="{my}" width="{rw - 32}" height="38" rx="9" fill="{ROW_BG}"/>'
            f'<text x="{rx + 30}" y="{my + 24}" font-family="{MONO}" font-size="13.5" '
            f'font-weight="700" fill="{BLUE_DARK}">{escape(m_name)}</text>'
            f'<text x="{rx + 148}" y="{my + 24}" font-size="13" fill="{SUBINK}">{escape(m_desc)}</text>'
        )

    # Execute arrow down to Workspaces card
    parts.append(
        f'<path d="M240 406 V438" fill="none" stroke="{BLUE}" stroke-width="1.6" '
        'marker-end="url(#arrowN)"/>'
    )
    exec_label = "执行" if zh else "Execute"
    ew = 46 if zh else 62
    parts.append(
        f'<rect x="250" y="411" width="{ew}" height="20" rx="10" fill="#ffffff" '
        f'stroke="{HOST_STROKE}" stroke-width="1"/>'
        f'<text x="{250 + ew / 2}" y="425" font-size="11.5" font-weight="700" '
        f'fill="{BLUE_DARK}" text-anchor="middle">{escape(exec_label)}</text>'
    )

    # Workspaces card (x=30..450, y=442..522)
    wx, wy, ww, wh = 30, 442, 420, 80
    parts.append(
        f'<rect x="{wx}" y="{wy}" width="{ww}" height="{wh}" rx="15" '
        f'fill="#ffffff" stroke="{CARD_STROKE}" stroke-width="1" filter="url(#cardShadowN)"/>'
    )
    parts.append(icon_svg("workspace", wx + 32, wy + 40))
    parts.append(
        f'<text x="{wx + 58}" y="{wy + 35}" font-size="16" font-weight="700" fill="{INK}">'
        f'{escape("工作区 · 文件与工具链" if zh else "Workspaces · Files & toolchains")}</text>'
    )
    parts.append(
        f'<text x="{wx + 58}" y="{wy + 57}" font-size="13" fill="{MUTED}">'
        f'{escape("在明确授权范围内执行本地仓库与工具" if zh else "Runs local repos & tools within explicit authorization")}</text>'
    )

    parts.append("</g></svg>")
    return "\n".join(parts) + "\n"


def render(lang: str, narrow: bool) -> None:
    zh = lang == "zh"
    width, height = (480, 550) if narrow else (1200, 350)
    svg = render_narrow(zh) if narrow else render_wide(zh)
    stem = f"runtime-architecture-{lang}{'-narrow' if narrow else ''}"
    svg_path = OUT / f"{stem}.svg"
    png_path = OUT / f"{stem}.png"
    svg_path.write_text(svg, encoding="utf-8")
    export_png(svg, png_path, scale=2)
    print(f"{stem}: {width}x{height} SVG, {width * 2}x{height * 2} PNG")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--language", choices=("zh", "en", "all"), default="all")
    args = parser.parse_args()
    for language in (("zh", "en") if args.language == "all" else (args.language,)):
        for is_narrow in (False, True):
            render(language, is_narrow)
