# Historical six-page layout reference. Not used by build.py.
from pathlib import Path
import json
from reportlab.pdfgen import canvas
from reportlab.lib.colors import HexColor
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import Paragraph
from reportlab.lib.styles import ParagraphStyle

ROOT = Path(__file__).resolve().parent
OUT = ROOT / 'holon-lite-paper-v3.pdf'
ROOT.mkdir(parents=True, exist_ok=True)
pdfmetrics.registerFont(TTFont('CN', '/System/Library/Fonts/STHeiti Light.ttc', subfontIndex=1))
pdfmetrics.registerFont(TTFont('CNB', '/System/Library/Fonts/STHeiti Medium.ttc', subfontIndex=1))
pdfmetrics.registerFont(TTFont('Latin', '/System/Library/Fonts/Supplemental/Arial.ttf'))
pdfmetrics.registerFont(TTFont('LatinB', '/System/Library/Fonts/Supplemental/Arial Bold.ttf'))
pdfmetrics.registerFont(TTFont('Mono', '/System/Library/Fonts/Monaco.ttf'))
W, H = 595.276, 841.89
BG = '#F7F7F2'; INK = '#142E36'; MUTE = '#587078'; TEAL = '#087F77'
LINE = '#D5DFDC'; PALE = '#E5F1EB'; WHITE = '#FFFFFF'; GOLD = '#B5792B'; SAND = '#FBF0DC'
BLUE = '#4C68A7'; LBLUE = '#EBEFF8'
c = canvas.Canvas(str(OUT), pagesize=(W, H))
c.setTitle('Holon | 让 Agent 随事件行动，持续推进工作 | Lite Paper v0.3')
c.setAuthor('Holon')
c.setSubject('事件驱动的 Agent 工作台：Webhook、定时任务、模板、技能、跨项目与团队协作')
audit = []
page = 0

def rect(x,y,w,h,fill,stroke=None,r=0):
    c.setFillColor(HexColor(fill))
    c.setStrokeColor(HexColor(stroke or fill))
    c.setLineWidth(.7)
    if r: c.roundRect(x,H-y-h,w,h,r,stroke=bool(stroke),fill=1)
    else: c.rect(x,H-y-h,w,h,stroke=bool(stroke),fill=1)

def line(x1,y1,x2,y2,color=LINE,width=1):
    c.setStrokeColor(HexColor(color)); c.setLineWidth(width)
    c.line(x1,H-y1,x2,H-y2)

def text(s,x,y,size=11,color=INK,font='CN'):
    c.setFillColor(HexColor(color)); c.setFont(font,size)
    c.drawString(x,H-y-size*.85,s)
    width=pdfmetrics.stringWidth(s,font,size)
    assert x+width <= W-20, (page,s,x+width)
    audit.append(dict(page=page,text=s,x=x,y=y,w=width,h=size))

def para(s,x,y,w,size=11,leading=None,color=INK,font='CN',limit=None):
    st=ParagraphStyle('p',fontName=font,fontSize=size,leading=leading or size*1.6,
                      textColor=HexColor(color),wordWrap='CJK')
    p=Paragraph(s,st); _,h=p.wrap(w,800)
    if limit: assert h<=limit,(page,s,h,limit)
    assert y+h<789,(page,s,y+h)
    p.drawOn(c,x,H-y-h)
    audit.append(dict(page=page,text=s,x=x,y=y,w=w,h=h))
    return h

def pill(s,x,y,color=TEAL,bg=PALE,w=None):
    w=w or pdfmetrics.stringWidth(s,'CN',9)+18
    rect(x,y,w,22,bg,r=11); text(s,x+9,y+6,9,color)

def arrow(x1,y1,x2,y2,color=TEAL):
    import math
    line(x1,y1,x2,y2,color,1.3)
    a=math.atan2(y2-y1,x2-x1)
    for d in [-.52,.52]:
        line(x2,y2,x2-6*math.cos(a+d),y2-6*math.sin(a+d),color,1.3)

def base(n,section):
    global page
    page=n
    rect(0,0,W,H,BG)
    rect(40,34,9,9,TEAL,r=2)
    text('holon',56,29,19,INK,'LatinB')
    text('LITE PAPER / v0.3',345,34,9,MUTE)
    line(40,62,W-40,62)
    line(40,796,W-40,796)
    text('HOLON  /  事件驱动，持续工作',40,809,8,MUTE)
    text(f'{section}   /   {n:02d}',416,809,8,MUTE)

def heading(kicker,title,sub):
    text(kicker,40,86,9,TEAL,'LatinB')
    text(title,40,110,27,INK,'CNB')
    para(sub,40,155,515,11,17,MUTE)

def note(label,body,y=693,h=76):
    rect(40,y,515,h,PALE,r=8)
    text(label,55,y+14,11,TEAL,'CNB')
    para(body,55,y+36,482,10.5,16,INK,limit=h-42)

def end(): c.showPage()

def link(s,url,x,y,size=8.5,color=TEAL):
    text(s,x,y,size,color)
    w=pdfmetrics.stringWidth(s,'CN',size)
    c.linkURL(url,(x,H-y-size-3,x+w,H-y+2),relative=0)

def summary(label,body,y,h=71):
    rect(40,y,515,h,PALE,r=8)
    text(label,55,y+12,11,TEAL,'CNB')
    para(body,55,y+33,484,10,15,limit=h-37)

# Typeset digest of the six body sections in ../holon-lite-paper.md, v0.3.
# Editorial notes and unmeasured resource claims are deliberately excluded.
base(1,'产品定位')
text('EVENT-DRIVEN AGENTS. CONTINUOUS WORK.',40,88,9,TEAL,'LatinB')
text('让 Agent 随事件行动，',40,123,30,INK,'CNB')
text('持续推进工作。',40,168,35,INK,'CNB')
para('接入你的系统，按事件与时间主动工作。',40,228,515,16,24,TEAL)
para('CI 失败后开始调查，新日志到达后整理问题，到了约定时间主动回查。配置好职责、工具与触发条件，工作就能由系统发起，无需你逐次打开对话、复制材料、再发出指令。',40,271,510,12,20)

rect(40,355,515,230,INK,r=10)
text('三个入口，同一个持续工作的现场',57,374,14,WHITE,'CNB')
for yy,a,b,col in [(413,'系统事件','CI / 日志 / 业务系统','#67D3BD'),(463,'时间触发','定时回查 / 周期巡检','#F0C679'),(513,'人的委托','Web / TUI / API','#B4C7EF')]:
    c.setFillColor(HexColor(col));c.circle(62,H-(yy+7),4,stroke=0,fill=1)
    text(a,76,yy,11,WHITE,'CNB');text(b,76,yy+21,9,'#C3D9D3')
    arrow(246,yy+14,298,yy+14,col)
rect(314,415,222,129,'#23434A',r=8)
text('有明确职责的 Agent',330,430,15,WHITE,'CNB')
text('读取事件与已有工作状态',330,461,10,'#C3D9D3')
text('开始新工作 / 恢复已有工作',330,486,10,WHITE)
text('执行与验证，交付或等待',330,511,10,'#C3D9D3')
text('Webhook 连接系统变化；定时任务让 Agent 主动回查。',57,559,10,'#C3D9D3')

text('把职责交给它，需要时再接入。',40,613,17,INK,'CNB')
para('Holon 是为个人与团队提供的 Agent 工作台。每个 Agent 保留自己的角色、技能与工作记录，使用宿主机上的项目和工具。你可以长期交给它审阅、跨项目交付或团队验收等职责。',40,651,515,11,18)
para('主动性来自预先配置的职责与触发条件。Agent 结合上下文判断下一步，在授权范围内执行，需要人决定时再交回。',40,725,515,10,16,MUTE)
text('个人电脑 / 远程开发机 / 团队服务器     ·     Apache-2.0 开源',40,773,9,MUTE)
end()

base(2,'功能地图')
heading('01 / CAPABILITIES','主动工作，有完整的运行支撑。','从触发入口，到角色配置、工作跟进与执行环境，Holon 把这些能力放在一起。')

groups=[
 ('事件与时间，让工作自动发起',[
  ('Webhook 外部触发','接收事件内容或唤醒信号，连接 CI、日志与业务系统。'),
  ('定时与周期触发','按约定时间回查、巡检和汇总，无需逐次人工发起。'),
  ('有状态的持续处理','结合已有职责和工作记录，启动新任务或继续原有工作。')]),
 ('角色与方法，可以反复使用',[
  ('长期存在的 Agent','独立 AgentHome，保存角色约定、记忆和工作记录。'),
  ('Agent Templates','从角色说明和技能引用创建 Agent，复用团队工作约定。'),
  ('按 Agent 管理 Skills','分别启用或禁用技能，为不同角色配置所需工作方法。'),
  ('模型选择','设置默认模型或单个 Agent 的模型覆盖，按任务选择。')]),
 ('工作进展，有明确记录',[
  ('WorkItem 工作记录','保存目标、计划、待办、阻塞与完成报告。'),
  ('显式等待与恢复','记录等待对象和原因，在任务结果、事件或反馈到来后继续。'),
  ('任务与子 Agent 监督','检查状态与输出、补充输入、停止任务并接回结果。'),
  ('来源与交付可追溯','保留输入来源，区分执行记录和面向用户的完成报告。')]),
 ('环境与入口，由你组织',[
  ('多 Workspace / worktree','跨项目切换工作区，为编码任务组织独立工作目录。'),
  ('Web GUI / TUI / API','浏览器、终端与 HTTP 接口连接同一个持续运行的后台。'),
  ('自托管与一体化部署','Rust 二进制内嵌 Web GUI，可在个人电脑或服务器运行。')]),
]
yy=199
for title,rows in groups:
    rect(40,yy,515,25,PALE,r=4)
    text(title,51,yy+7,11,TEAL,'CNB')
    yy+=32
    for a,b in rows:
        text(a,48,yy+3,10,INK,'CNB')
        para(b,206,yy+1,340,9.5,13,limit=26)
        yy+=28
    yy+=10
para('模板定义角色的起点，Skills 提供可复用的方法，WorkItem 承载具体目标。外部服务按需配置工具、凭据和事件接入。',40,753,515,9.5,15,MUTE)
end()

base(3,'持续审阅')
heading('02 / WEBHOOK TO ACTION','CI 一有结果，Agent 就有下一步。','适合个人与团队：配置一次审阅职责，让新 PR、新提交与 CI 事件持续带来工作。')
pill('应用流程示意',40,197,MUTE,'#E9EDEB')
rect(40,237,515,87,WHITE,LINE,r=8)
text('给 reviewer 的持续职责',55,251,11,TEAL,'CNB')
para('“负责这个仓库的 PR 审阅。收到新 PR 后开始检查，新提交或 CI 结果到来后继续跟进；有争议时等我决定，为每个 PR 留下结论和验证依据。”',55,276,483,11,17)

rect(40,348,148,52,INK,r=7)
text('GitHub / CI',55,359,12,WHITE,'LatinB')
text('投递事件与 PR 线索',55,382,9,'#C3D9D3')
arrow(197,374,311,374)
text('webhook',222,352,10,TEAL,'Latin')
rect(322,348,233,52,PALE,r=7)
text('reviewer 接收并核对状态',336,360,12,TEAL,'CNB')
text('新 PR 开始工作，已有 PR 继续跟进',336,383,9,MUTE)

steps=[
 ('01','建立工作项','记录审阅范围、检查计划与验收条件。',TEAL),
 ('02','检查与调查','阅读改动、执行验证；CI 失败时调查原因。',TEAL),
 ('03','等待与恢复','等待新提交、CI 或人工决定，保留阻塞与进度。',GOLD),
 ('04','完成交付','汇总审阅版本、问题处理与验证依据。',TEAL)]
line(57,445,57,628,LINE,1.5)
for i,(n,title,body,col) in enumerate(steps):
    y=428+i*59
    rect(40,y,34,28,PALE if col==TEAL else SAND,r=5)
    text(n,49,y+8,10,col,'LatinB')
    text(title,92,y+1,12,INK,'CNB')
    para(body,92,y+25,455,10.5,16,MUTE)
summary('从反复发起请求，到持续承担职责','固定角色持续接收事件，每项 PR 的目标与进度独立保留。关闭终端后，可以从浏览器回到原来的 Agent 与工作项。',686,73)
para('前提：GitHub 操作工具、凭据与事件接入，宿主机和后台保持可用。需要筛选或转换事件时可接入适配器；合并和发布需有相应授权。',40,769,515,8,11,MUTE)
end()

base(4,'跨项目交付')
heading('03 / MULTIPLE WORKSPACES','一个目标，协调多个项目。','适合服务端、SDK、应用和文档的联动修改；角色与方法可以沿用到不同仓库。')
pill('应用流程示意',40,197,MUTE,'#E9EDEB')
rect(40,237,515,74,WHITE,LINE,r=8)
text('你的目标',55,250,11,TEAL,'CNB')
para('“为接口增加一个字段，同步更新 SDK 和示例应用。分别验证兼容性；先整理方案，影响旧版本的决定等我确认。”',55,274,483,11,17)

rect(40,337,515,56,INK,r=8)
text('开发 Agent',57,351,14,WHITE,'CNB')
text('Templates 初始化角色 · Skills 配置方法 · WorkItem 保存目标',57,375,9,'#C3D9D3')
arrow(297,393,297,419)
for x,title,sub in [(40,'服务端','修改接口与测试'),(218,'SDK','更新调用与类型'),(397,'示例应用','调整用法与验证')]:
    rect(x,429,158,76,WHITE,LINE,r=7)
    text(title,x+14,444,14,TEAL,'CNB')
    text(sub,x+14,475,10,MUTE)
arrow(199,467,215,467);arrow(377,467,394,467)
text('同一 Agent 绑定多个工作区，按步骤切换当前执行环境。',40,520,10,MUTE)

rect(40,555,246,85,LBLUE,r=7)
text('可独立推进的部分',54,570,12,BLUE,'CNB')
para('委托子 Agent 做独立复核或文档检查，跟踪状态并接回结果。',54,595,218,10,15)
rect(306,555,249,85,PALE,r=7)
text('需要并行修改时',320,570,12,TEAL,'CNB')
para('使用独立 worktree，分别保留代码改动与验证依据。',320,595,220,10,15)
summary('交付时，回答每个项目发生了什么','汇总改动、检查结果、兼容性问题和待确认事项。主 Agent 检查各项输出，用户据此决定是否接受交付。',676,76)
para('边界：一个 Agent 同时只有一个当前工作区；多工作区支持绑定与切换。worktree 组织独立改动，安全隔离需要另行配置。',40,766,515,8.5,12,MUTE)
end()

base(5,'团队共享')
heading('04 / SHARED AGENTS','让共享 Agent 接住团队交接。','部署到团队服务器，长期负责调查、审阅和验收，把结果留在共同使用的记录里。')
pill('依据已有团队实践整理',40,197,TEAL,PALE)
para('一个四人 AI 硬件团队将 Holon 部署到服务器。调查 Agent 接收设备日志，测试 Agent 跟进修复后的版本与反馈；开发者继续使用熟悉的编码工具。',40,237,515,11,18)

rows=[
 ('事件','设备日志经 webhook 到达','调查 Agent 读取获准 trace，整理线索并创建或补充 Issue。',TEAL,PALE),
 ('协作','开发者接手问题并修复','团队沿用已有 Issue 与 PR 记录，保留调查依据和修改结果。',BLUE,LBLUE),
 ('等待','代码合并，继续跟进可测试版本','测试 Agent 保留待验收工作，等待对应部署或客户端发包。',GOLD,SAND),
 ('验收','自动检查，与人工实测汇合','核对版本和修复，汇总 Agent 检查及真机反馈，更新验收记录。',TEAL,PALE)]
line(58,333,58,560,LINE,1.5)
for i,(tag,title,body,col,bg) in enumerate(rows):
    y=314+i*73
    pill(tag,40,y,col,bg,w=46)
    text(title,104,y+2,12,INK,'CNB')
    para(body,104,y+28,444,10.5,16,MUTE)

text('共享职责、项目环境与未完成工作。',40,621,15,INK,'CNB')
para('换一个成员接手，仍能使用已有线索与工作记录。部署或人工反馈尚未就绪时，事情仍有明确归属。',40,651,515,10.5,16)
rect(40,698,515,58,PALE,r=7)
text('定时触发也能主动回查',54,710,11,TEAL,'CNB')
para('例如，周期检查待验收修复与服务状态，发现变化后继续处理。此项为应用示例。',54,733,484,9.5,14)
link('案例来源：一个小团队的 Agent Native 实践',
     'https://holon.run/zh-CN/blog/agents-in-a-small-team',40,768,8)
text('依据既有记录整理；未在本次重新审计。集成与访问范围由部署者配置。',40,782,7.5,MUTE)
end()

base(6,'开始使用')
heading('05 / GET STARTED','选一项职责，接入一个触发条件。','从一个 Agent 和一件容易验收的工作开始，再扩展到更多项目与团队角色。')
text('01  在你的机器上运行',40,204,13,INK,'CNB')
para('Rust 二进制内嵌 Web GUI，使用发布构建无需另外部署前端。可运行在个人电脑、远程开发机或团队服务器。',40,232,515,10.5,17)
rect(40,287,515,121,INK,r=8)
for i,s in enumerate(['brew tap holon-run/tap && brew install holon','holon onboard','holon daemon start','holon tui']):
    text(s,56,304+i*23,10,'#E2F5ED','Mono')
para('也可从 GitHub Releases 下载二进制。启动后台后，打开本地 Web GUI，或使用 TUI 创建 Agent、管理技能和工作区。',40,419,515,9.5,15,MUTE)

text('02  配置角色、工作环境与触发入口',40,473,13,INK,'CNB')
for i,(a,b) in enumerate([
 ('角色','从模板创建 Agent，配置职责与所需 Skills。'),
 ('环境','接入工作区，配置模型、工具和必要凭据。'),
 ('触发','接入一个 webhook，或配置一次性 / 周期定时任务。'),
 ('验收','明确范围与交付标准，检查结果，必要时补充决定。')]):
    y=507+i*30
    pill(a,40,y,TEAL,PALE,w=46)
    para(b,102,y+3,451,10.5,16)

text('03  让它开始承担一件具体工作',40,652,13,INK,'CNB')
para('可以从“CI 失败后调查原因”或“定期检查待验收事项”开始。记录目标与进度，遇到需要人决定的问题时等待，完成后交付产物与验证依据。',40,680,515,10.5,17)
para('运行前提：主机与后台保持可用；工具和事件接入按需配置。数据流向取决于模型与工具，本地执行使用宿主机权限。',40,731,515,8.5,13,MUTE)
link('GitHub 项目与下载','https://github.com/holon-run/holon',40,772,9)
link('使用文档 holon.run','https://holon.run',222,772,9)
text('v0.3 / 2026-09-17',432,772,8,MUTE)
end()

c.save()
import hashlib
source=ROOT.parent/'holon-lite-paper.md'
qa=ROOT.parent.parent/'tmp/pdfs/holon-lite-paper-v3'
qa.mkdir(parents=True,exist_ok=True)
(qa/'layout.json').write_text(json.dumps(audit,ensure_ascii=False,indent=2))
(qa/'source.json').write_text(json.dumps({'source':str(source),'sha256':hashlib.sha256(source.read_bytes()).hexdigest(),'version':'v0.3','pages':6},ensure_ascii=False,indent=2))
print(OUT)
