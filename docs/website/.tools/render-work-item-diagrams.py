#!/usr/bin/env python3
"""Export the Chinese WorkItem article diagrams as SVG sources and 2x PNGs.

Requires CairoSVG and Noto Sans CJK SC. These are conceptual illustrations,
not runtime state-machine or component-boundary specifications.
"""

from pathlib import Path
from xml.sax.saxutils import escape

import cairosvg


OUT = Path(__file__).resolve().parents[1] / "assets"
INK = "#183457"
MUTED = "#53677d"
BLUE = "#285fa7"
BORDER = "#d9e2eb"
PAPER = "#f8fafc"
ACTIVE = ("#eef4ff", BLUE)
WAITING = ("#fff6e7", "#865b1c")
DONE = ("#eaf5f0", "#27664d")


class Diagram:
    width = 800

    def __init__(self, title, subtitle, height, width=800):
        self.width = width
        self.height = height
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">',
            f'<title id="title">{escape(title)}</title>',
            f'<desc id="desc">{escape(subtitle)}</desc>',
            '<defs><marker id="arrow" markerWidth="8" markerHeight="8" '
            'refX="7" refY="4" orient="auto-start-reverse">'
            f'<path d="M1 1L7 4L1 7" fill="none" stroke="{BLUE}" '
            'stroke-width="1.4"/></marker></defs>',
            f'<rect width="{width}" height="{height}" rx="8" fill="{PAPER}"/>',
            '<g font-family="Noto Sans CJK SC, sans-serif">',
        ]
        self.text(40, 45, title, 22, bold=True)

    def text(self, x, y, value, size=18, color=INK, bold=False, anchor="start"):
        self.parts.append(
            f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" '
            f'font-weight="{600 if bold else 400}" text-anchor="{anchor}">'
            f'{escape(value)}</text>'
        )

    def rect(self, x, y, w, h, fill="#fff", stroke=BORDER, radius=6):
        self.parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" '
            f'fill="{fill}" stroke="{stroke}"/>'
        )

    def path(self, data, arrow=True, dashed=False, color=BLUE):
        marker = 'arrow'
        if arrow and color != BLUE:
            marker = f'arrow-{len(self.parts)}'
            self.parts.append(
                f'<defs><marker id="{marker}" markerWidth="8" markerHeight="8" '
                'refX="7" refY="4" orient="auto-start-reverse">'
                f'<path d="M1 1L7 4L1 7" fill="none" stroke="{color}" '
                'stroke-width="1.4"/></marker></defs>'
            )
        attrs = f' marker-end="url(#{marker})"' if arrow else ''
        attrs += ' stroke-dasharray="5 7"' if dashed else ''
        self.parts.append(
            f'<path d="{data}" fill="none" stroke="{color}" '
            f'stroke-width="1.3"{attrs}/>'
        )

    def save(self, name):
        source = '\n'.join(self.parts + ['</g></svg>'])
        (OUT / f'{name}.svg').write_text(source, encoding='utf-8')
        cairosvg.svg2png(
            bytestring=source.encode(), write_to=str(OUT / f'{name}.png'),
            output_width=self.width * 2, output_height=self.height * 2,
        )


def architecture():
    d = Diagram('WorkItem：工作状态的保存与恢复', '判断与执行按轮进行，工作记录跨轮保留。', 430)

    for x, title, note in [
        (40, 'Agent 判断', '决定下一步'),
        (460, '运行时', '保存状态 · 安排执行'),
    ]:
        d.rect(x, 82, 300, 68)
        d.text(x + 20, 109, title, 18, bold=True)
        d.text(x + 20, 133, note, 15, MUTED)
    d.path('M340 116H458')
    d.path('M610 150V189')

    d.rect(40, 190, 720, 102, ACTIVE[0], '#c4d6ec')
    d.text(60, 220, 'WorkItem', 20, BLUE, bold=True)
    d.text(182, 220, '持久工作记录', 15, MUTED)
    d.path('M60 236H740', arrow=False, color='#d4e0ee')
    for x, label in [(60, '目标与交付'), (300, '进度与证据'), (540, '等待与恢复')]:
        d.text(x, 267, label, 18)

    d.path('M400 292V343', dashed=True)
    d.text(414, 321, '读取工作记录', 14, MUTED)
    for i, (title, note) in enumerate([
        ('信号到达', '检查恢复条件'),
        ('恢复上下文', '接回这项工作'),
        ('继续推进', '复查并更新记录'),
    ]):
        x = 40 + i * 260
        d.rect(x, 344, 200, 62)
        d.text(x + 100, 370, title, 18, bold=True, anchor='middle')
        d.text(x + 100, 392, note, 14, MUTED, anchor='middle')
        if i < 2:
            d.path(f'M{x + 200} 375H{x + 258}')
    d.save('work-item-architecture-zh')


def sequence():
    d = Diagram(
        '一个 Agent 如何切换并推进两个 WorkItem',
        '每个 PR 对应一个 WorkItem。同一个 reviewer 的当前工作焦点依次为 A、B、A、B。'
        '外部事件使对应 WorkItem 可恢复，再由运行时安排执行；切换焦点不清除等待记录。',
        484,
    )
    teal = '#34746d'
    d.text(40, 80, '一个 PR 对应一个 WorkItem · 横向为时间', 14, MUTED)

    d.text(40, 135, '外部事件', 15, MUTED)
    for x, width, title, note, color in [
        (310, 160, 'PR A 提交修订', 'WorkItem A 可恢复', BLUE),
        (610, 150, 'PR B 的 CI 完成', 'WorkItem B 可恢复', teal),
    ]:
        d.rect(x, 108, width, 58)
        d.text(x + width / 2, 131, title, 15, anchor='middle')
        d.text(x + width / 2, 153, note, 13, color, anchor='middle')
    d.path('M390 166V185H530V238', dashed=True)
    d.path('M685 166V204H690V238', dashed=True, color=teal)
    d.text(40, 211, '可恢复后，由运行时安排执行，不立即抢占', 13, MUTED)

    d.text(28, 257, '同一个 reviewer', 14, bold=True)
    d.text(28, 282, '当前 WorkItem', 13, MUTED)
    for i, (center, title, note, fill, color) in enumerate([
        (210, 'WorkItem A', '首次审阅', ACTIVE[0], BLUE),
        (370, 'WorkItem B', '首次审阅', '#edf6f3', teal),
        (530, 'WorkItem A', '复查并交付', ACTIVE[0], BLUE),
        (690, 'WorkItem B', '检查 CI 结果', '#edf6f3', teal),
    ]):
        d.rect(center - 66, 240, 132, 66, fill, fill)
        d.text(center, 266, title, 16, color, bold=True, anchor='middle')
        d.text(center, 291, note, 14, MUTED, anchor='middle')
        if i < 3:
            d.path(f'M{center + 67} 276H{center + 92}')
            d.text(center + 80, 258, '切换', 13, MUTED, anchor='middle')

    d.text(28, 354, 'WorkItem A', 14, BLUE)
    d.text(28, 375, '保留工作记录', 13, MUTED)
    d.path('M210 306V366H362M386 366H530', arrow=False, color=BLUE)
    d.path('M530 360V372', arrow=False, color=BLUE)
    d.text(226, 354, '等待修订', 14, BLUE)
    # A small bridge keeps the crossing distinct from a state hand-off.
    d.path('M370 306V359Q382 366 370 373V420H690', arrow=False, color=teal)
    d.path('M690 414V426', arrow=False, color=teal)
    d.text(28, 408, 'WorkItem B', 14, teal)
    d.text(28, 429, '保留工作记录', 13, MUTED)
    d.text(386, 408, '等待 CI', 14, teal)

    d.text(40, 462, '当前 WorkItem 是工作焦点，不是运行状态；切换焦点不清除另一项工作的记录。', 13, MUTED)
    d.save('work-item-reviewer-sequence-zh')


if __name__ == '__main__':
    architecture()
    sequence()
