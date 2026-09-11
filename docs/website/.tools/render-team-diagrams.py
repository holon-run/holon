"""Render the small-team article's diagrams from reviewed, anonymized aggregates."""

import math
from pathlib import Path
from xml.sax.saxutils import escape

ASSETS = Path(__file__).resolve().parents[1] / "assets"
INK, MUTED, BLUE, LINE = "#183457", "#536b87", "#285fa7", "#c4d3e6"
BG, HUMAN = "#f7faff", "#936022"
# Retained model-round metadata, 2026-06-25 through 2026-09-09 (Beijing).
# Only role aggregates are public; private source records are not site inputs.
ROLES = [
    ("代码审阅", 4_049_694_990, BLUE),
    ("现场调查", 1_922_634_581, "#398a88"),
    ("测试验收", 826_219_378, "#bd8131"),
    ("协作助手", 334_697_563, "#56643e"),
    ("数据分析", 334_504_443, "#c35672"),
    ("产品运维", 296_309_708, "#75579d"),
]
TOTAL = sum(value for _, value, _ in ROLES)
assert TOTAL == 7_764_060_663


class Diagram:
    def __init__(self, width, height, title, description):
        self.narrow = width == 480
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">',
            f'<title id="title">{escape(title)}</title><desc id="desc">{escape(description)}</desc>',
            '<defs><marker id="arrow" markerWidth="9" markerHeight="9" refX="8" refY="4.5" '
            f'orient="auto"><path d="M1 1 L8 4.5 L1 8" fill="none" stroke="{BLUE}" '
            'stroke-width="1.5"/></marker></defs>',
            f'<rect width="{width}" height="{height}" rx="16" fill="{BG}"/>',
            f'<g font-family="Noto Sans CJK SC, sans-serif" fill="{INK}">',
        ]

    def text(self, x, y, value, size=22, color=INK, weight=400, anchor="start"):
        if self.narrow:
            size = max(size, 22)
        self.parts.append(
            f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" '
            f'font-weight="{weight}" text-anchor="{anchor}">{escape(value)}</text>'
        )

    def rect(self, x, y, w, h, fill="white", stroke=LINE, radius=10):
        self.parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" '
            f'fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>'
        )

    def path(self, d, color=BLUE, arrow=False, dashed=False):
        self.parts.append(
            f'<path d="{d}" fill="none" stroke="{color}" stroke-width="2"'
            + (' marker-end="url(#arrow)"' if arrow else "")
            + (' stroke-dasharray="6 6"' if dashed else "")
            + "/>"
        )

    def icon(self, kind, x, y, color=BLUE):
        self.parts.append(f'<g transform="translate({x},{y})">')
        if kind == "phone":
            self.rect(8, 0, 40, 64, stroke=color, radius=7)
            self.path("M20 8 H36 M23 56 H33", color)
            self.path("M17 35 L25 43 L40 23", color)
        elif kind == "server":
            for top in (0, 23, 46):
                self.rect(0, top, 64, 17, stroke=color, radius=3)
                self.path(f"M8 {top+8} H12 M20 {top+8} H52", color)
        elif kind == "report":
            self.path("M6 0 H40 L58 18 V64 H6 Z M40 0 V18 H58", color)
            for top, end in ((29, 46), (40, 38), (51, 46)):
                self.path(f"M17 {top} H{end}", color)
        elif kind == "issue":
            self.rect(0, 0, 64, 64, stroke=color)
            self.parts.append(f'<circle cx="17" cy="18" r="6" fill="none" stroke="{color}" stroke-width="2"/>')
            self.path("M31 18 H53 M12 36 H51 M12 48 H43", color)
        elif kind == "piano":
            self.rect(0, 12, 72, 47, stroke=color, radius=3)
            for k in (18, 36, 54):
                self.path(f"M{k} 12 V59", color)
            for k in (13, 31, 49):
                self.rect(k, 12, 10, 26, fill=color, stroke=color, radius=1)
            self.parts.append('<circle cx="27" cy="4" r="4" fill="#398a88"/>')
        self.parts.append("</g>")

    def save(self, name):
        (ASSETS / name).write_text("\n".join(self.parts + ["</g></svg>"]) + "\n")


def usage(narrow):
    width, height = (480, 890) if narrow else (1200, 570)
    d = Diagram(width, height, "77 天累计 Token 用量与角色占比",
                "2026年6月25日至9月9日，六类共享Agent累计77.64亿Token，含缓存输入。饼图按角色划分，不代表费用或成果。")
    d.text(30, 48, "77 天，共享 Agent 的累计用量", 26, weight=600)
    d.text(30, 82, "2026.06.25 至 09.09 · 北京时间", 20, MUTED)
    d.text(30, 146, "77.64 亿 Token", 42, weight=600)
    d.text(30, 183, "输入 76.94 亿 · 输出 0.70 亿", 22)
    d.text(30, 215, "含缓存读取 62.59 亿，已计入输入", 20, MUTED)
    cx, cy, radius = (240, 395, 140) if narrow else (260, 390, 132)
    angle = -math.pi / 2
    for _, value, color in ROLES:
        end = angle + value / TOTAL * math.tau
        x1, y1 = cx + radius * math.cos(angle), cy + radius * math.sin(angle)
        x2, y2 = cx + radius * math.cos(end), cy + radius * math.sin(end)
        large = int(end - angle > math.pi)
        d.parts.append(
            f'<path d="M{cx} {cy} L{x1:.3f} {y1:.3f} A{radius} {radius} 0 {large} 1 '
            f'{x2:.3f} {y2:.3f} Z" fill="{color}" stroke="{BG}" stroke-width="2"/>'
        )
        angle = end
    lx, ly, step = (30, 580, 39) if narrow else (530, 155, 55)
    if not narrow:
        d.text(lx, 110, "按角色划分", 24, weight=600)
        d.text(930, 110, "亿 Token", 20, MUTED, anchor="end")
        d.text(1135, 110, "占比", 20, MUTED, anchor="end")
    for i, (label, value, color) in enumerate(ROLES):
        y = ly + i * step
        d.rect(lx, y-17, 17, 17, color, color, 3)
        d.text(lx+30, y, label, 22)
        d.text(325 if narrow else 930, y, f"{value/1e8:.2f}", 22, anchor="end")
        d.text(450 if narrow else 1135, y, f"{value/TOTAL:.2%}", 22, anchor="end")
    if narrow:
        d.text(30, 826, "图例数值：亿 Token / 占比", 19, MUTED)
        d.text(30, 866, "仅留存轮次；不代表费用或成果", 19, MUTED)
    else:
        d.path("M530 464 H1140", LINE)
        d.text(530, 503, "模型使用规模，不是费用或成果指标", 22, MUTED)
        d.text(530, 539, "仅留存轮次；数值各自四舍五入", 20, MUTED)
    d.save(f"team-usage-zh{'-narrow' if narrow else ''}.svg")


def investigation(narrow):
    d = Diagram(480 if narrow else 1200, 1150 if narrow else 690,
                "从客户端上报到 GitHub Issue",
                "客户端生成诊断report并上报服务器；webhook通知Holon内的trace report Agent，读取获准trace、整理现场和证据，创建或补充GitHub Issue，再交开发者定位修复。")
    d.text(28, 48, "从客户端上报到 GitHub Issue", 27, weight=600)
    d.text(28, 84, "日志进入调查，调查留下可接手的记录", 20, MUTED)
    if narrow:
        rows = [
            (118, "phone", "客户端", "用户操作与系统响应"),
            (278, "report", "诊断 report", "带上现场描述与 trace 线索"),
            (438, "server", "服务器", "接收上报，提供获准访问的 trace"),
        ]
        for y, icon, title, detail in rows:
            d.icon(icon, 35, y+8)
            d.text(130, y+30, title, 25, weight=600)
            # The server description wraps to keep the mobile type readable.
            if icon == "server":
                d.text(130, y+64, "接收上报", 21)
                d.text(130, y+100, "提供获准访问的 trace", 21)
            else:
                d.text(130, y+65, detail, 20)
        for y, label in ((206, "生成报告"), (366, "上传 report"), (560, "webhook 通知")):
            d.path(f"M65 {y} V{y+48}", arrow=True)
            d.text(88, y+32, label, 20, MUTED)
        d.rect(28, 626, 424, 258, "#e8f0fb", "#90afd4")
        d.text(50, 662, "Holon · 常驻 Runtime", 24, weight=600)
        d.text(50, 698, "trace report Agent", 25, BLUE, 600)
        steps = ["读取获准 trace，还原操作过程",
                 "整理卡住位置、取消与恢复线索",
                 "注明服务端日志缺口与待确认点"]
        for i, label in enumerate(steps):
            d.text(50, 747+i*42, f"{i+1:02}  {label}", 21)
        d.path("M65 884 V932", arrow=True)
        d.text(88, 916, "创建 / 补充", 20, MUTED)
        d.icon("issue", 35, 956)
        d.text(130, 979, "GitHub Issue", 25, weight=600)
        d.text(130, 1014, "场景、证据、标签、负责人", 20)
        d.text(28, 1074, "→ 开发者继续定位与修复", 24, BLUE, 600)
        d.text(28, 1120, "调查线索 ≠ 已确认根因", 20, MUTED)
    else:
        # Artifact flow runs left to right; investigation expands inside Holon.
        nodes = [
            (75, "phone", "客户端", "记录操作与响应"),
            (305, "report", "诊断 report", "现场描述 + trace 线索"),
            (555, "server", "服务器", "接收上报 / 保存现场"),
        ]
        for x, icon, title, detail in nodes:
            d.icon(icon, x, 145)
            d.text(x+30, 252, title, 25, weight=600, anchor="middle")
            d.text(x+30, 288, detail, 20, MUTED, anchor="middle")
        for start, end, label in ((146, 284, "生成报告"), (378, 534, "上传 report")):
            d.path(f"M{start} 177 H{end}", arrow=True)
            d.text((start+end)/2, 151, label, 19, MUTED, anchor="middle")
        d.rect(758, 120, 410, 286, "#e8f0fb", "#90afd4")
        d.text(786, 159, "Holon · 常驻 Runtime", 24, weight=600)
        d.text(786, 197, "trace report Agent", 27, BLUE, 600)
        for i, label in enumerate(("读取获准 trace，还原操作过程",
                                   "整理卡住位置与取消 / 恢复线索",
                                   "注明日志缺口，保留待确认点")):
            d.text(786, 252+i*52, f"{i+1:02}", 20, BLUE, 600)
            d.text(824, 252+i*52, label, 20)
        d.path("M630 177 H746", arrow=True)
        d.text(692, 147, "webhook", 19, MUTED, anchor="middle")
        d.text(692, 208, "通知调查", 18, MUTED, anchor="middle")
        d.path("M590 302 V380 H746", arrow=True, dashed=True)
        d.text(600, 344, "获准 trace", 19, MUTED)
        d.text(600, 370, "供 Agent 读取", 18, MUTED)
        d.path("M961 406 V456", arrow=True)
        d.text(985, 440, "创建 / 补充", 20, MUTED)
        d.rect(550, 470, 618, 116)
        d.icon("issue", 574, 493)
        d.text(664, 510, "GitHub Issue", 26, weight=600)
        d.text(664, 548, "场景 · 证据 · 待确认点 · 标签 · 负责人", 21)
        d.path("M550 528 H435", arrow=True)
        d.text(492, 506, "交接", 19, MUTED, anchor="middle")
        d.text(72, 510, "开发者继续定位与修复", 26, weight=600)
        d.text(72, 548, "不用重新下载日志、整理现场", 21, MUTED)
        d.path("M28 622 H1172", LINE)
        d.text(28, 658, "调查线索 ≠ 已确认根因；已有 Issue 的结果补回原记录。", 22, MUTED)
    d.save(f"team-investigation-zh{'-narrow' if narrow else ''}.svg")


def collaboration(narrow):
    d = Diagram(480 if narrow else 1200, 1700 if narrow else 1080,
                "Issue 关闭之后，按版本持续跟进验收",
                "PR 合并、关联 Issue 关闭不等于已部署、已发包或验收通过。测试 Agent 等待部署和发布事件，按每项变更的依赖核对实际交付版本，整理变更与已关闭 Issue 的验收清单；未进入版本的修复继续等待。自动检查或人工实测之后，注明版本、范围和来源，写回 Issue 与版本验收记录。")
    d.text(28, 48, "Issue 关闭，验收还没结束", 28, weight=600)
    d.text(28, 84, "跟进一个版本周期，而非一次合并", 21, MUTED)
    if narrow:
        d.rect(28, 116, 424, 118)
        d.text(48, 153, "多个 PR 合并 → 关联 Issue 关闭", 23, weight=600)
        d.text(48, 190, "代码已合并，修复尚未部署或发包", 22)
        d.path("M240 234 V274", arrow=True)
        d.rect(28, 280, 424, 426, fill="#edf4ff", stroke=BLUE)
        d.text(48, 319, "测试 Agent 持续跟进", 25, BLUE, 600)
        d.text(48, 355, "等待部署 / 发布事件，工作仍保留", 22)
        d.rect(48, 378, 384, 88)
        d.text(68, 413, "服务器部署完成", 24, weight=600)
        d.text(68, 447, "服务器改动：核对对应部署", 22, MUTED)
        d.rect(48, 482, 384, 88)
        d.text(68, 517, "客户端版本发布", 24, weight=600)
        d.text(68, 551, "客户端改动：核对对应版本", 22, MUTED)
        d.text(48, 611, "按每项变更的依赖等待", 23, BLUE, 600)
        d.text(48, 647, "不是每项都要同时等两个事件", 22)
        d.text(48, 681, "跨端问题核对两端条件", 22, MUTED)
        d.path("M240 706 V744", arrow=True)
        d.rect(28, 750, 424, 180)
        d.text(48, 790, "核对版本，形成验收清单", 25, weight=600)
        d.text(48, 830, "本次实际交付的变更", 22)
        d.text(48, 864, "+ 这些变更关联的已关闭 Issue", 22)
        d.text(48, 904, "不按 Issue 关闭日期机械归集", 22, MUTED)
        d.text(28, 966, "已进入可测版本的清单项", 23, BLUE, 600)
        d.path("M240 986 V1030", arrow=True)
        d.text(28, 1068, "清单内的两种验证方式", 25, weight=600)
        d.rect(28, 1092, 424, 140)
        d.text(48, 1130, "Agent 自动检查", 24, BLUE, 600)
        d.text(48, 1168, "如后台跳转：4 类请求 → 响应", 22)
        d.text(48, 1203, "记录检查范围，不外推其他功能", 22, MUTED)
        d.rect(28, 1250, 424, 140)
        d.text(48, 1288, "人工实测，Agent 接回结果", 24, HUMAN, 600)
        d.text(48, 1326, "如琴键发声：修复包 + 对应固件", 22)
        d.text(48, 1361, "人工可先完成，无需重复派测", 22, MUTED)
        d.path("M240 1390 V1430", arrow=True)
        d.rect(28, 1436, 424, 144, fill="#edf4ff", stroke=BLUE)
        d.text(48, 1477, "回写原 Issue 与版本验收记录", 24, BLUE, 600)
        d.text(48, 1516, "版本 · 检查范围 · 结果来源", 22)
        d.text(48, 1552, "分别记录自动检查和人工结论", 22)
        d.text(28, 1624, "未进入可测版本的修复继续等待；", 22, MUTED)
        d.text(28, 1660, "缺少必要反馈，也不能判定通过。", 22, MUTED)
    else:
        d.rect(28, 116, 1144, 102)
        d.text(52, 157, "多个 PR 合并", 25, weight=600)
        d.text(52, 193, "一个版本周期内的修复", 21, MUTED)
        d.path("M330 164 H392", arrow=True)
        d.text(420, 157, "关联 Issue 关闭", 25, weight=600)
        d.text(420, 193, "代码完成，验收仍待跟进", 21, MUTED)
        d.path("M760 134 V199", LINE)
        d.text(792, 157, "尚未部署 / 尚未发包", 25, HUMAN, 600)
        d.text(792, 193, "不能据此判定验收通过", 21, MUTED)
        d.path("M600 218 V255", arrow=True)
        d.rect(28, 262, 1144, 266, fill="#edf4ff", stroke=BLUE)
        d.text(52, 306, "测试 Agent 持续跟进，等待部署 / 发布事件", 27, BLUE, 600)
        d.rect(52, 330, 524, 100)
        d.icon("server", 72, 348)
        d.text(159, 370, "服务器部署完成", 25, weight=600)
        d.text(159, 407, "服务器改动：核对对应部署", 22, MUTED)
        d.rect(624, 330, 524, 100)
        d.icon("phone", 645, 348)
        d.text(730, 370, "客户端版本发布", 25, weight=600)
        d.text(730, 407, "客户端改动：核对对应版本", 22, MUTED)
        d.text(52, 470, "按每项变更的依赖等待，不是每项都要同时等两个事件；跨端问题核对两端条件。", 22)
        d.text(52, 506, "等待期间，待验收的工作仍然保留。", 21, MUTED)
        d.path("M600 528 V565", arrow=True)
        d.rect(28, 572, 1144, 130)
        d.text(52, 615, "核对版本，形成验收清单", 26, weight=600)
        d.text(52, 655, "本次实际交付的变更 + 关联的已关闭 Issue", 23)
        d.text(52, 684, "不按 Issue 关闭日期机械归集", 20, MUTED)
        d.path("M730 591 V682", LINE)
        d.text(760, 618, "尚未进入可测版本的修复", 23, BLUE, 600)
        d.text(760, 658, "继续等待，不算本次已验收", 22, MUTED)
        d.path("M600 702 V739", arrow=True)
        d.text(28, 779, "验收清单中的两种验证方式", 25, weight=600)
        for x in (28, 624):
            d.rect(x, 802, 548, 130)
        d.text(52, 841, "Agent 自动检查", 25, BLUE, 600)
        d.text(52, 878, "如后台跳转：4 类请求 → 检查响应", 22)
        d.text(52, 912, "记录检查范围，不外推其他功能", 21, MUTED)
        d.text(648, 841, "人工实测，Agent 接回结果", 25, HUMAN, 600)
        d.text(648, 878, "如琴键发声：修复包 + 对应固件", 22)
        d.text(648, 912, "人工可先完成，无需重复派测", 21, MUTED)
        d.path("M302 932 V952 H600 V971", arrow=True)
        d.path("M898 932 V952 H600")
        d.rect(28, 978, 1144, 74, fill="#edf4ff", stroke=BLUE)
        d.text(52, 1024, "回写原 Issue 与版本验收记录", 25, BLUE, 600)
        d.text(680, 1024, "注明版本、检查范围和结果来源", 23)
    d.save(f"team-collaboration-zh{'-narrow' if narrow else ''}.svg")


if __name__ == "__main__":
    for narrow in (False, True):
        usage(narrow)
        investigation(narrow)
        collaboration(narrow)
    print(f"Rendered six SVGs; total={TOTAL:,}; role shares={sum(v/TOTAL for _, v, _ in ROLES):.0%}")
