#!/usr/bin/env python3
"""Export the Chinese WorkItem article diagrams as SVG sources and 2x PNGs.

Requires CairoSVG and Noto Sans CJK SC.
"""

from pathlib import Path
from xml.sax.saxutils import escape

import cairosvg


OUT = Path(__file__).resolve().parents[1] / "assets"
INK = "#183457"
MUTED = "#536b87"
BLUE = "#285fa7"


class Diagram:
    def __init__(self, title, height):
        self.height = height
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="800" height="{height}" '
            f'viewBox="0 0 800 {height}" role="img">',
            f"<title>{escape(title)}</title>",
            '<defs><marker id="arrow" markerWidth="8" markerHeight="8" '
            'refX="6" refY="3" orient="auto-start-reverse">'
            f'<path d="M0 0L6 3L0 6" fill="none" stroke="{BLUE}"/>'
            "</marker></defs>",
            f'<rect width="800" height="{height}" rx="16" fill="#f7faff"/>',
            '<g font-family="Noto Sans CJK SC, sans-serif">',
        ]
        self.text(32, 42, title, 24, bold=True)

    def text(self, x, y, text, size=18, color=INK, bold=False, anchor="start"):
        self.parts.append(
            f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" '
            f'font-weight="{600 if bold else 400}" text-anchor="{anchor}">'
            f"{escape(text)}</text>"
        )

    def box(self, x, y, w, h, title, lines, fill="#fff"):
        self.parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="8" '
            f'fill="{fill}" stroke="#c4d3e6" stroke-width="1.5"/>'
        )
        self.text(x + 18, y + 30, title, 20, bold=True)
        for i, line in enumerate(lines):
            self.text(x + 18, y + 58 + i * 26, line, 17, MUTED)

    def path(self, d, arrow=True, dashed=False):
        attrs = ' marker-end="url(#arrow)"' if arrow else ""
        attrs += ' stroke-dasharray="5 6"' if dashed else ""
        self.parts.append(
            f'<path d="{d}" fill="none" stroke="{BLUE}" '
            f'stroke-width="1.6"{attrs}/>'
        )

    def save(self, name):
        source = "\n".join(self.parts + ["</g></svg>"])
        (OUT / f"{name}.svg").write_text(source, encoding="utf-8")
        cairosvg.svg2png(
            bytestring=source.encode(),
            write_to=str(OUT / f"{name}.png"),
            output_width=1600,
            output_height=self.height * 2,
        )


def architecture():
    d = Diagram("WorkItem：连接判断、状态与后续执行", 660)
    d.box(32, 76, 330, 112, "LLM · 当前轮次", [
        "理解目标、分析证据、决定下一步",
        "通过工具表达等待、切换与完成",
    ])
    d.box(438, 76, 330, 112, "运行时 · 状态转换", [
        "校验工具请求与工作归属",
        "保存等待、续接与完成结果",
    ])
    d.path("M362 130H438")
    d.box(32, 256, 736, 148, "围绕同一 WorkItem 保存，但分开承担责任", [
        "工作记录：目标、生命周期、计划引用、进度",
        "调度依据：等待条件、续接关系、焦点及阻碍信息",
        "恢复材料：计划正文、审阅发现、提交标识、证据引用",
    ], "#edf4ff")
    d.path("M603 188V256")
    d.text(617, 226, "持久保存", 16, MUTED)
    d.box(32, 466, 330, 112, "上下文装配", [
        "目标与进度 → 相关历史和证据",
        "按预算提供下一轮的恢复依据",
    ])
    d.box(438, 466, 330, 112, "调度判断", [
        "结合工作状态与到达的信号",
        "判断等待、可运行或续接",
    ])
    d.path("M197 404V466")
    d.path("M603 404V466")
    d.path("M438 522H362")
    d.text(385, 508, "续办", 15, MUTED)
    d.path("M32 522H16V130H32")
    d.text(32, 623, "模型判断业务是否满足；运行时不从自然语言里猜测调度状态。", 18)
    d.save("work-item-architecture-zh")


def sequence():
    d = Diagram("Reviewer：A 与 B 如何交错，而不互相覆盖", 790)
    d.text(32, 72, "示意顺序；不是固定优先级，也不是即时抢占", 17, MUTED)
    xs = [132, 400, 668]
    for x, title in zip(xs, ["WorkItem A · #101", "Reviewer / 调度", "WorkItem B · #102"]):
        d.text(x, 120, title, 18, bold=True, anchor="middle")
        d.path(f"M{x} 142V698", arrow=False, dashed=True)

    def event(y, start, end, label):
        d.parts.append(
            f'<rect x="32" y="{y - 33}" width="736" height="27" fill="#f7faff"/>'
        )
        d.text(400, y - 12, label, 17, anchor="middle")
        d.path(f"M{start} {y}H{end}")

    event(180, 400, 132, "审阅 A，记录权限问题")
    event(249, 132, 400, "A 登记外部等待，让出执行")
    event(318, 400, 668, "开始 B，读取 diff 并分析")
    event(387, 132, 400, "A 更新通知到达；不立即抢占 B")
    event(456, 668, 400, "B 保存进度，登记测试 task 等待")
    event(525, 400, 132, "调度允许时恢复 A，查询新提交与 CI")
    event(594, 132, 400, "A 验收后交付；B 仍未完成")
    event(663, 668, 400, "B 测试结果到达，后续恢复 B")
    d.box(32, 717, 736, 52, "A 与 B 各有目标、进度、等待条件和完成边界", [])
    d.save("work-item-reviewer-sequence-zh")


if __name__ == "__main__":
    architecture()
    sequence()
