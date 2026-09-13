#!/usr/bin/env python3
"""Export WorkItem diagrams as SVG sources and 2x PNGs (--lang zh|en).

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


LANG = "zh"
EN = {
    "WorkItem：工作状态的保存与恢复": "WorkItem: save and resume work",
    "判断与执行按轮进行，工作记录跨轮保留。": "Decisions and execution happen in rounds; work records persist across rounds.",
    "持久工作记录": "Persistent work record",
    "读取工作记录": "Read work record",
    "一个 reviewer，两个 WorkItem": "One reviewer, two WorkItems",
    "每个 PR 对应一个 WorkItem。同一个 reviewer 的当前工作焦点依次为 A、B、A、B。外部事件使对应 WorkItem 可恢复，再由运行时安排执行；切换焦点不清除等待记录。": "Each PR has a WorkItem. The same reviewer focuses on A, B, A, then B. External events make the corresponding WorkItem resumable; the runtime schedules execution. Switching focus does not erase wait records.",
    "外部事件": "External events",
    "唤醒": "Wake",
    "当前 WorkItem": "Current WorkItem",
    "等待记录": "Wait records",
    "A · 等待修订": "A · Await revisions",
    "B · 等待 CI": "B · Await CI",
    "Agent 判断": "Agent reasoning",
    "决定下一步": "Choose the next step",
    "运行时": "Runtime",
    "保存状态 · 安排执行": "Save state · Schedule execution",
    "目标与交付": "Goals & delivery",
    "进度与证据": "Progress & evidence",
    "等待与恢复": "Wait & resume",
    "PR A 更新": "PR A updated",
    "PR B · CI 完成": "PR B · CI done",
    "信号到达": "Signal arrives",
    "检查恢复条件": "Check resume conditions",
    "恢复上下文": "Restore context",
    "接回这项工作": "Pick up this work",
    "继续推进": "Continue work",
    "复查并更新记录": "Review and update record",
    "审阅": "Review",
    "复查交付": "Verify delivery",
    "检查 CI": "Check CI",
    "切换": "Switch"
}

def translate(value):
    if LANG == "zh":
        return value
    # Numbered investigation steps are assembled before reaching text().
    if value[:2].isdigit() and value[2:4] == "  ":
        return value[:4] + translate(value[4:])
    if any("\u4e00" <= char <= "\u9fff" for char in value):
        return EN[value]
    return value


class Diagram:
    width = 800

    def __init__(self, title, subtitle, height, width=800):
        self.width = width
        self.height = height
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">',
            f'<title id="title">{escape(translate(title))}</title>',
            f'<desc id="desc">{escape(translate(subtitle))}</desc>',
            '<defs><marker id="arrow" markerWidth="8" markerHeight="8" '
            'refX="7" refY="4" orient="auto-start-reverse">'
            f'<path d="M1 1L7 4L1 7" fill="none" stroke="{BLUE}" '
            'stroke-width="1.4"/></marker></defs>',
            f'<rect width="{width}" height="{height}" rx="8" fill="{PAPER}"/>',
            '<g font-family="Noto Sans CJK SC, sans-serif">',
        ]
        self.text(40, 45, title, 22, bold=True)

    def text(self, x, y, value, size=18, color=INK, bold=False, anchor="start"):
        if LANG == "en":
            import cairocffi as cairo

            context = cairo.Context(cairo.ImageSurface(cairo.FORMAT_ARGB32, 1, 1))
            context.select_font_face("Noto Sans CJK SC", 0, int(bold))
            context.set_font_size(size)
            width = context.text_extents(translate(value))[4]
            available = self.width - x - 28
            if anchor == "middle":
                available = 180 if y in (370, 392) else 112
                if y == 121:
                    available = 140
                if y == 218:
                    available = 38
            elif x == 28:
                available = 114
            elif y == 267:
                available = 215
            if width > available:
                size *= available / width
        self.parts.append(
            f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" '
            f'font-weight="{600 if bold else 400}" text-anchor="{anchor}">'
            f'{escape(translate(value))}</text>'
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
        name = name.replace("-zh", f"-{LANG}")
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
        '一个 reviewer，两个 WorkItem',
        '每个 PR 对应一个 WorkItem。同一个 reviewer 的当前工作焦点依次为 A、B、A、B。'
        '外部事件使对应 WorkItem 可恢复，再由运行时安排执行；切换焦点不清除等待记录。',
        410,
    )
    teal = '#34746d'
    d.text(28, 121, '外部事件', 14, MUTED)
    for x, width, title, color in [
        (310, 160, 'PR A 更新', BLUE),
        (610, 150, 'PR B · CI 完成', teal),
    ]:
        d.rect(x, 96, width, 40)
        d.text(x + width / 2, 121, title, 15, color, anchor='middle')
    d.path('M390 136V163H530V198', dashed=True)
    d.path('M685 136V163H690V198', dashed=True, color=teal)
    d.text(544, 184, '唤醒', 13, BLUE)
    d.text(704, 184, '唤醒', 13, teal)

    d.text(28, 239, '当前 WorkItem', 13, MUTED)
    for i, (center, title, note, fill, color) in enumerate([
        (210, 'WorkItem A', '审阅', ACTIVE[0], BLUE),
        (370, 'WorkItem B', '审阅', '#edf6f3', teal),
        (530, 'WorkItem A', '复查交付', ACTIVE[0], BLUE),
        (690, 'WorkItem B', '检查 CI', '#edf6f3', teal),
    ]):
        d.rect(center - 60, 200, 120, 66, fill, fill)
        d.text(center, 226, title, 16, color, bold=True, anchor='middle')
        d.text(center, 251, note, 14, MUTED, anchor='middle')
        if i < 3:
            d.path(f'M{center + 61} 236H{center + 98}')
            d.text(center + 80, 218, '切换', 13, MUTED, anchor='middle')

    d.text(28, 331, '等待记录', 14, MUTED)
    d.path('M210 266V326H362M386 326H530', arrow=False, color=BLUE)
    d.path('M530 320V332', arrow=False, color=BLUE)
    d.text(226, 314, 'A · 等待修订', 14, BLUE)
    # A small bridge keeps the crossing distinct from a state hand-off.
    d.path('M370 266V319Q382 326 370 333V376H690', arrow=False, color=teal)
    d.path('M690 370V382', arrow=False, color=teal)
    d.text(386, 364, 'B · 等待 CI', 14, teal)

    d.save('work-item-reviewer-sequence-zh')


if __name__ == '__main__':
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lang", choices=("zh", "en"), default="zh")
    LANG = parser.parse_args().lang
    architecture()
    sequence()
