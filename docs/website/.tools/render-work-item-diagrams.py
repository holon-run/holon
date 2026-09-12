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
    d = Diagram('一轮结束，工作仍在', '判断与执行按轮进行，工作记录跨轮保留。', 430)

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
    d = Diagram('两项工作，各自接着做', '同一个 reviewer 交错推进两项工作；横向为一种示意顺序。', 356)
    xs = [176 + i * 104 for i in range(6)]
    d.text(40, 80, '同一个 reviewer · 按 01—06 顺序阅读', 14, MUTED)
    d.path('M130 106H756', color=BORDER)
    for i, x in enumerate(xs):
        d.text(x, 96, f'0{i + 1}', 13, MUTED, anchor='middle')

    for y, label, pr in [(148, '工作 A', 'PR #101'), (245, '工作 B', 'PR #102')]:
        d.text(40, y + 20, label, 18, bold=True)
        d.text(40, y + 43, pr, 14, MUTED)
        d.path(f'M130 {y + 28}H756', arrow=False, color=BORDER)

    def stage(step, y, title, note='', palette=ACTIVE, span=1):
        x = xs[step] - 47
        width = 94 + (span - 1) * 104
        fill, color = palette
        d.rect(x, y, width, 58, fill, fill, radius=5)
        center = x + width / 2
        d.text(center, y + (25 if note else 35), title, 16, color, anchor='middle')
        if note:
            d.text(center, y + 46, note, 13, MUTED, anchor='middle')

    stage(0, 148, '审阅', '记录问题')
    stage(1, 148, '等待修订', palette=WAITING)
    stage(2, 148, '更新到达', '暂存信号', WAITING)
    stage(3, 148, '恢复审阅', '复查 CI')
    stage(4, 148, '完成交付', palette=DONE, span=2)

    d.text(xs[0], 280, '未开始', 15, MUTED, anchor='middle')
    stage(1, 245, '审阅')
    stage(2, 245, '等待测试', '保留进度', WAITING, span=3)
    stage(5, 245, '恢复', '测试结果到达')

    d.text(40, 334, '切换工作，不等于结束另一项工作。', 15, MUTED)
    d.save('work-item-reviewer-sequence-zh')


if __name__ == '__main__':
    architecture()
    sequence()
