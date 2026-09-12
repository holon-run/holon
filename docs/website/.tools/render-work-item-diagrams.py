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
        attrs = ' marker-end="url(#arrow)"' if arrow else ''
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
        '一个 Agent 如何交错推进两项工作',
        '同一个 reviewer 依次审阅 A、审阅 B、恢复 A、恢复 B。'
        '外部事件满足恢复条件后，由运行时安排执行；等待期间工作记录保留。',
        438,
    )
    teal = '#34746d'
    d.text(40, 80, '一种处理顺序 · 横向为时间', 14, MUTED)

    d.text(40, 135, '外部事件', 15, MUTED)
    for x, width, title in [(322, 136, '作者提交修订'), (628, 124, 'CI 完成')]:
        d.rect(x, 108, width, 40)
        d.text(x + width / 2, 133, title, 15, anchor='middle')
    d.path('M390 148V168H530V206', dashed=True)
    d.path('M690 148V206', dashed=True)
    d.text(40, 182, '满足恢复条件后，由运行时安排执行', 13, MUTED)

    d.text(40, 233, '同一个 Agent', 15, bold=True)
    d.text(40, 256, 'reviewer', 14, MUTED)
    for i, (center, title, note, fill, color) in enumerate([
        (210, '审阅 A', '记录问题', ACTIVE[0], BLUE),
        (370, '审阅 B', '检查测试', '#edf6f3', teal),
        (530, '恢复 A', '复查并交付', ACTIVE[0], BLUE),
        (690, '恢复 B', '接着检查', '#edf6f3', teal),
    ]):
        d.rect(center - 62, 208, 124, 62, fill, fill)
        d.text(center, 233, title, 17, color, bold=True, anchor='middle')
        d.text(center, 256, note, 14, MUTED, anchor='middle')
        if i < 3:
            d.path(f'M{center + 62} 239H{center + 96}')

    d.text(40, 319, '保留工作记录', 15, MUTED)
    d.text(40, 341, '等待不占执行线', 13, MUTED)
    d.path('M210 270V324H362M386 324H530', arrow=False, color=BLUE)
    d.path('M530 318V330', arrow=False, color=BLUE)
    d.text(226, 312, 'A 等待修订', 15, BLUE)
    # A small bridge keeps the crossing distinct from a state hand-off.
    d.path('M370 270V317Q382 324 370 331V374H690', arrow=False, color=teal)
    d.path('M690 368V380', arrow=False, color=teal)
    d.text(386, 362, 'B 等待测试', 15, teal)

    d.text(40, 412, '中间实线：执行顺序    虚线：事件触发恢复条件，不表示立即抢占', 13, MUTED)
    d.save('work-item-reviewer-sequence-zh')


if __name__ == '__main__':
    architecture()
    sequence()
