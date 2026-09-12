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
    width = 720

    def __init__(self, title, subtitle, height, width=720):
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
            f'<rect width="{width}" height="{height}" rx="16" fill="{PAPER}"/>',
            '<g font-family="Noto Sans CJK SC, sans-serif">',
        ]
        self.text(32, 49, title, 29, bold=True)
        self.text(32, 84, subtitle, 21, MUTED)

    def text(self, x, y, value, size=23, color=INK, bold=False, anchor="start"):
        self.parts.append(
            f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" '
            f'font-weight="{600 if bold else 400}" text-anchor="{anchor}">'
            f'{escape(value)}</text>'
        )

    def rect(self, x, y, w, h, fill="#fff", stroke=BORDER, radius=10):
        self.parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" '
            f'fill="{fill}" stroke="{stroke}"/>'
        )

    def path(self, data, arrow=True, dashed=False, color=BLUE):
        attrs = ' marker-end="url(#arrow)"' if arrow else ''
        attrs += ' stroke-dasharray="5 7"' if dashed else ''
        self.parts.append(
            f'<path d="{data}" fill="none" stroke="{color}" '
            f'stroke-width="1.8"{attrs}/>'
        )

    def save(self, name):
        source = '\n'.join(self.parts + ['</g></svg>'])
        (OUT / f'{name}.svg').write_text(source, encoding='utf-8')
        cairosvg.svg2png(
            bytestring=source.encode(), write_to=str(OUT / f'{name}.png'),
            output_width=self.width * 2, output_height=self.height * 2,
        )


def architecture():
    d = Diagram('一轮结束，工作仍在', '执行可以暂停 · 记录跨轮保留 · 有依据地继续', 736)

    # Current execution: judgment becomes recorded state, not a prose promise.
    for x, title, line1, line2 in [
        (32, 'Agent 判断', '分析目标与证据', '决定下一步'),
        (400, '运行时登记', '保存进度与等待', '安排后续执行'),
    ]:
        d.rect(x, 117, 288, 145)
        d.text(x + 24, 155, title, 26, bold=True)
        d.text(x + 24, 198, line1, 23, MUTED)
        d.text(x + 24, 234, line2, 23, MUTED)
    d.path('M320 186H399')
    d.path('M544 262V306')

    # The persistent record is the dominant element, not an implementation box.
    d.rect(32, 307, 656, 193, ACTIVE[0], '#b6cbe7')
    d.text(56, 348, 'WorkItem', 29, BLUE, bold=True)
    d.text(256, 348, '属于这项工作的持久记录', 23, BLUE)
    d.path('M56 369H664', arrow=False, color='#c4d6ec')
    columns = [(56, '目标与交付', '做到什么算完成'),
               (262, '进度与证据', '已经确认了什么'),
               (468, '等待与恢复', '何时回来查什么')]
    for x, title, line in columns:
        d.text(x, 412, title, 25, bold=True)
        d.text(x, 455, line, 22, MUTED)

    d.text(32, 548, '等待之后，接着做', 24, bold=True)
    d.text(378, 548, '提供恢复依据', 21, MUTED)
    for i, (title, subtitle) in enumerate([
        ('信号到达', '检查恢复条件'),
        ('恢复上下文', '读取工作记录'),
        ('下一轮判断', '复查并更新记录'),
    ]):
        x = 32 + i * 232
        d.rect(x, 577, 192, 101)
        d.text(x + 96, 615, title, 25, bold=True, anchor='middle')
        d.text(x + 96, 653, subtitle, 22, MUTED, anchor='middle')
        if i < 2:
            d.path(f'M{x + 192} 628H{x + 230}')
    d.path('M360 500V576', dashed=True)
    d.text(32, 716, '信号让工作有机会继续；验收仍需检查证据。', 22, MUTED)
    d.save('work-item-architecture-zh')


def sequence():
    d = Diagram('两项工作，各自接着做', '同一个 reviewer · 从上往下读 · 一种示意顺序', 994)
    left, right, width = 80, 400, 288
    d.rect(left, 116, width, 56, '#e5edf8', '#e5edf8')
    d.rect(right, 116, width, 56, '#e5edf8', '#e5edf8')
    d.text(left + 144, 153, 'A / PR #101', 26, bold=True, anchor='middle')
    d.text(right + 144, 153, 'B / PR #102', 26, bold=True, anchor='middle')
    d.path('M46 204V874', arrow=False, color=BORDER)

    def cell(x, y, title, note, palette=None):
        fill, color = palette or (PAPER, MUTED)
        d.rect(x, y, width, 98, fill, BORDER if palette else PAPER)
        d.text(x + 20, y + 36, title, 25, color, bold=True)
        d.text(x + 20, y + 74, note, 22, MUTED)

    rows = [
        (('审阅 A', '记录权限问题', ACTIVE),
         ('尚未开始', '独立的另一项委托', None)),
        (('等待修订', '进度留在 A', WAITING),
         ('审阅 B', '读取代码并分析', ACTIVE)),
        (('更新信号到达', '先记录，不立即抢占', WAITING),
         ('保存 B，等待测试', '保留发现与结果入口', WAITING)),
        (('恢复 A', '复查修订与当前 CI', ACTIVE),
         ('继续等待测试', 'A 的更新不解除等待', WAITING)),
        (('A 完成交付', '在约定授权范围内', DONE),
         ('仍未完成', '进度与等待继续保留', WAITING)),
        (('保留交付结果', 'A 的责任已结束', DONE),
         ('结果到达，恢复 B', '检查结果，继续推进', ACTIVE)),
    ]
    for i, (a, b) in enumerate(rows):
        y = 192 + i * 118
        d.rect(31, y + 31, 30, 30, '#e5edf8', '#e5edf8', 15)
        d.text(46, y + 54, str(i + 1), 19, BLUE, bold=True, anchor='middle')
        cell(left, y, *a)
        cell(right, y, *b)
        if i < len(rows) - 1:
            for x in (left + width / 2, right + width / 2):
                d.path(f'M{x} {y + 98}V{y + 116}', arrow=False, color=BORDER)
    d.text(80, 938, '蓝色：推进中   琥珀色：等待   绿色：已交付', 22, MUTED)
    d.text(80, 975, '切换工作，不等于结束另一项工作。', 24, bold=True)
    d.save('work-item-reviewer-sequence-zh')


def narrow_architecture():
    d = Diagram('一轮结束，工作仍在', '执行暂停，工作记录仍然保留', 1018, width=480)
    for y, title, note in [
        (117, 'Agent 判断', '分析证据，决定下一步'),
        (261, '运行时登记', '保存状态，安排后续执行'),
    ]:
        d.rect(32, y, 416, 108)
        d.text(56, y + 41, title, 27, bold=True)
        d.text(56, y + 80, note, 24, MUTED)
        d.path(f'M240 {y + 108}V{y + 143}')
    d.rect(32, 405, 416, 267, ACTIVE[0], '#b6cbe7')
    d.text(56, 450, 'WorkItem / 持久工作记录', 27, BLUE, bold=True)
    for y, title, note in [
        (496, '目标与交付', '做到什么算完成'),
        (559, '进度与证据', '已经确认了什么'),
        (622, '等待与恢复', '何时回来查什么'),
    ]:
        d.text(56, y, title, 24, bold=True)
        d.text(56, y + 29, note, 23, MUTED)
    d.text(32, 722, '等待之后，接着做', 26, bold=True)
    for i, (title, note) in enumerate([
        ('信号到达', '检查恢复条件'),
        ('恢复上下文', '读取工作记录'),
        ('下一轮判断', '复查并更新记录'),
    ]):
        y = 750 + i * 72
        d.text(48, y + 27, str(i + 1), 24, BLUE, bold=True)
        d.text(86, y + 27, title, 25, bold=True)
        d.text(86, y + 57, note, 23, MUTED)
        if i < 2:
            d.path(f'M55 {y + 38}V{y + 74}', arrow=False, color=BORDER)
    d.text(32, 997, '信号不等于验收通过。', 23, MUTED)
    d.save('work-item-architecture-zh-narrow')


def narrow_sequence():
    d = Diagram('两项工作，各自接着做', '同一个 reviewer · 一种示意顺序', 1050, width=480)
    d.text(32, 126, 'A / PR #101     B / PR #102', 24, bold=True)
    rows = [
        (('A 审阅，记录权限问题', ACTIVE), ('B 尚未开始', None)),
        (('A 等待作者修订', WAITING), ('B 审阅代码', ACTIVE)),
        (('A 更新到达，不立即抢占', WAITING), ('B 保存进度，等测试结果', WAITING)),
        (('A 恢复，复查修订与 CI', ACTIVE), ('B 继续等待测试结果', WAITING)),
        (('A 按授权完成交付', DONE), ('B 仍在等测试结果', WAITING)),
        (('A 保留交付结果', DONE), ('B 收到测试结果，恢复', ACTIVE)),
    ]
    for i, row in enumerate(rows):
        y = 152 + i * 136
        d.rect(32, y, 416, 118)
        d.text(51, y + 35, f'0{i + 1}', 21, MUTED, bold=True)
        for j, (text, palette) in enumerate(row):
            color = palette[1] if palette else MUTED
            d.text(96, y + 43 + j * 46, text, 24, color, bold=bool(palette))
        if i < len(rows) - 1:
            d.path(f'M64 {y + 118}V{y + 134}', arrow=False, color=BORDER)
    d.text(32, 997, '等待与完成，分别属于各自的工作。', 24, bold=True)
    d.text(32, 1030, '通知不会自动结束另一项工作。', 23, MUTED)
    d.save('work-item-reviewer-sequence-zh-narrow')


if __name__ == '__main__':
    architecture()
    sequence()
    narrow_architecture()
    narrow_sequence()
