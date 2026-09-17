"""Shared PDF typography and Markdown block rendering, without product copy."""

import html
import re

from reportlab.lib import colors
from reportlab.lib.styles import ParagraphStyle
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import (
    BaseDocTemplate,
    Flowable,
    Frame,
    PageBreak,
    PageTemplate,
    Paragraph,
    Table,
    TableStyle,
)

WIDTH, HEIGHT = 595.276, 841.89
MARGIN = 40
CONTENT_WIDTH = WIDTH - MARGIN * 2
INK = colors.HexColor('#142E36')
MUTED = colors.HexColor('#587078')
TEAL = colors.HexColor('#087F77')
PALE = colors.HexColor('#E5F1EB')
PAPER = colors.HexColor('#F7F7F2')
LINE = colors.HexColor('#D5DFDC')


def register_fonts(regular, bold):
    for name, path in [('Paper', regular), ('PaperBold', bold)]:
        index = 1 if path.name in ('STHeiti Light.ttc', 'STHeiti Medium.ttc') else 0
        pdfmetrics.registerFont(TTFont(name, str(path), subfontIndex=index))
    pdfmetrics.registerFontFamily(
        'Paper', normal='Paper', bold='PaperBold', italic='Paper', boldItalic='PaperBold'
    )


def styles(lang):
    base = dict(fontName='Paper', textColor=INK, wordWrap='CJK' if lang == 'zh-CN' else None)
    return {
        'body': ParagraphStyle('body', fontSize=10, leading=15.5, spaceAfter=5, **base),
        'title': ParagraphStyle('title', fontSize=27, leading=37, spaceAfter=18,
                                keepWithNext=True, **{**base, 'fontName': 'PaperBold'}),
        'section': ParagraphStyle('section', fontSize=22, leading=30, spaceAfter=16,
                                  keepWithNext=True, **{**base, 'fontName': 'PaperBold'}),
        'subsection': ParagraphStyle('subsection', fontSize=13, leading=20,
                                     spaceBefore=10, spaceAfter=7, keepWithNext=True,
                                     **{**base, 'fontName': 'PaperBold', 'textColor': TEAL}),
        'quote': ParagraphStyle('quote', fontSize=10.5, leading=17, **base),
        'note': ParagraphStyle('note', fontSize=9, leading=14, spaceAfter=10,
                               **{**base, 'textColor': MUTED}),
        'cell': ParagraphStyle('cell', fontSize=9, leading=14, **base),
        'table_head': ParagraphStyle('table_head', fontSize=9.5, leading=14,
                                     **{**base, 'fontName': 'PaperBold', 'textColor': TEAL}),
        'bullet': ParagraphStyle('bullet', fontSize=10, leading=16, leftIndent=12,
                                 firstLineIndent=-10, spaceAfter=3, **base),
    }


def public_url(url, source):
    if '://' in url or url.startswith('#'):
        return url
    target = (source.parent / url).resolve()
    website = source.parents[1] / 'website'
    if not target.exists():
        raise ValueError(f'Broken source link: {url}')
    if not target.is_relative_to(website):
        raise ValueError(f'No public PDF URL for local source link: {url}')
    route = target.relative_to(website).as_posix()
    if route.endswith('README.md'):
        route = route.removesuffix('README.md')
    else:
        route = route.removesuffix('.md')
    return 'https://holon.run/' + route


INLINE = re.compile(r'(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[[^\]]+\]\([^)]+\))')


def inline(value, source):
    result = []
    for part in INLINE.split(value):
        if part.startswith('**') and part.endswith('**'):
            result.append('<b>' + html.escape(part[2:-2]) + '</b>')
        elif part.startswith('`') and part.endswith('`'):
            result.append('<font color="#087F77">' + html.escape(part[1:-1]) + '</font>')
        elif part.startswith('*') and part.endswith('*'):
            result.append(html.escape(part[1:-1]))
        elif match := re.fullmatch(r'\[([^\]]+)\]\(([^)]+)\)', part):
            url = html.escape(public_url(match[2], source), quote=True)
            result.append(f'<link href="{url}" color="#087F77">{html.escape(match[1])}</link>')
        else:
            result.append(html.escape(part))
    return ''.join(result)


class CodeBlock(Flowable):
    """A bounded, non-splitting code/diagram block; fail rather than clip."""

    def __init__(self, lines):
        super().__init__()
        self.lines = lines
        self.spaceAfter = 12

    def wrap(self, available_width, available_height):
        self.width = available_width
        widest = max((pdfmetrics.stringWidth(s, 'Paper', 1) for s in self.lines), default=1)
        self.size = min(10, (available_width - 24) / max(widest, 1))
        if self.size < 8:
            raise ValueError('Code/diagram line is too long: shorten it in the Markdown source.')
        self.leading = self.size * 1.5
        self.height = len(self.lines) * self.leading + 24
        if self.height > HEIGHT - 160:
            raise ValueError('Code/diagram block is taller than a page; split it in Markdown.')
        return self.width, self.height

    def draw(self):
        self.canv.setFillColor(INK)
        self.canv.roundRect(0, 0, self.width, self.height, 7, fill=1, stroke=0)
        self.canv.setFillColor(colors.HexColor('#E2F5ED'))
        self.canv.setFont('Paper', self.size)
        for i, value in enumerate(self.lines):
            self.canv.drawString(12, self.height - 13 - self.size - i * self.leading, value)


def table_flow(lines, source, style):
    rows = [[cell.strip() for cell in row.strip().strip('|').split('|')] for row in lines]
    if len(rows) < 2 or not all(re.fullmatch(r':?-{3,}:?', cell) for cell in rows[1]):
        raise ValueError('Expected a Markdown table header separator.')
    rows.pop(1)
    count = len(rows[0])
    if any(len(row) != count for row in rows):
        raise ValueError('Markdown table columns do not match.')
    widths = ([112, (CONTENT_WIDTH - 112) / 2, (CONTENT_WIDTH - 112) / 2]
              if count == 3 else [CONTENT_WIDTH / count] * count)
    content = [[Paragraph(inline(cell, source), style['table_head' if i == 0 else 'cell'])
                for cell in row] for i, row in enumerate(rows)]
    table = Table(content, colWidths=widths, repeatRows=1, hAlign='LEFT', spaceAfter=14)
    table.setStyle(TableStyle([
        ('BACKGROUND', (0, 0), (-1, 0), PALE),
        ('VALIGN', (0, 0), (-1, -1), 'TOP'),
        ('LEFTPADDING', (0, 0), (-1, -1), 9),
        ('RIGHTPADDING', (0, 0), (-1, -1), 9),
        ('TOPPADDING', (0, 0), (-1, -1), 6),
        ('BOTTOMPADDING', (0, 0), (-1, -1), 6),
        ('LINEBELOW', (0, 0), (-1, -1), .5, LINE),
    ]))
    return table


def markdown_story(body, source, lang):
    """Support only the small Markdown subset documented in README.md."""
    style = styles(lang)
    lines = body.splitlines()
    story = []
    i = 0
    sections = 0
    while i < len(lines):
        line = lines[i].strip()
        if not line or line == '---':
            i += 1
            continue
        if line.startswith('```'):
            code = []
            i += 1
            while i < len(lines) and not lines[i].strip().startswith('```'):
                code.append(lines[i])
                i += 1
            if i == len(lines):
                raise ValueError('Unclosed code fence.')
            story.append(CodeBlock(code))
        elif line.startswith('|'):
            block = []
            while i < len(lines) and lines[i].strip().startswith('|'):
                block.append(lines[i])
                i += 1
            story.append(table_flow(block, source, style))
            continue
        elif match := re.match(r'^(#{1,3}) (.+)$', line):
            level = len(match[1])
            if level == 2:
                if sections:
                    story.append(PageBreak())
                sections += 1
            kind = {1: 'title', 2: 'section', 3: 'subsection'}[level]
            title = inline(match[2], source)
            if level == 1 and lang == 'zh-CN':
                title = title.replace('，', '，<br/>', 1)
            story.append(Paragraph(title, style[kind]))
        elif line.startswith('> '):
            block = []
            while i < len(lines) and lines[i].strip().startswith('> '):
                block.append(lines[i].strip()[2:])
                i += 1
            quote = Table([[Paragraph(inline(' '.join(block), source), style['quote'])]],
                          colWidths=[CONTENT_WIDTH], spaceBefore=3, spaceAfter=12)
            quote.setStyle(TableStyle([
                ('BACKGROUND', (0, 0), (-1, -1), PALE),
                ('LEFTPADDING', (0, 0), (-1, -1), 12),
                ('RIGHTPADDING', (0, 0), (-1, -1), 12),
                ('TOPPADDING', (0, 0), (-1, -1), 12),
                ('BOTTOMPADDING', (0, 0), (-1, -1), 12),
            ]))
            story.append(quote)
            continue
        elif line.startswith('- '):
            story.append(Paragraph('• ' + inline(line[2:], source), style['bullet']))
        elif line.startswith(('#', '<', '![')):
            raise ValueError(f'Unsupported Markdown block: {line[:80]}')
        else:
            block = []
            while i < len(lines) and lines[i].strip():
                value = lines[i].strip()
                if block and value.startswith(('#', '|', '>', '```', '- ')):
                    break
                hard_break = lines[i].endswith(('  ', '\\'))
                if value.endswith('\\'):
                    value = value[:-1]
                block.append(inline(value, source))
                if hard_break:
                    block.append('<br/>')
                i += 1
            kind = 'note' if line.startswith('*') and not line.startswith('**') else 'body'
            story.append(Paragraph(' '.join(block), style[kind]))
            continue
        i += 1
    return story


def render_pdf(body, source, metadata, output):
    doc = BaseDocTemplate(str(output), pagesize=(WIDTH, HEIGHT),
                         title=metadata['title'], author='Holon',
                         leftMargin=MARGIN, rightMargin=MARGIN,
                         topMargin=84, bottomMargin=60)

    def page_frame(canv, document):
        canv.saveState()
        canv.setFillColor(PAPER)
        canv.rect(0, 0, WIDTH, HEIGHT, fill=1, stroke=0)
        canv.setFillColor(TEAL)
        canv.roundRect(40, HEIGHT - 43, 9, 9, 2, fill=1, stroke=0)
        canv.setFillColor(INK)
        canv.setFont('PaperBold', 19)
        canv.drawString(56, HEIGHT - 46, 'holon')
        canv.setFont('Paper', 9)
        canv.setFillColor(MUTED)
        canv.drawRightString(WIDTH - 40, HEIGHT - 43,
                             f"LITE PAPER / {metadata['lang']} / v{metadata['version']}")
        canv.setStrokeColor(LINE)
        canv.line(40, HEIGHT - 62, WIDTH - 40, HEIGHT - 62)
        canv.line(40, 45, WIDTH - 40, 45)
        canv.setFont('Paper', 8)
        canv.drawString(40, 25, 'HOLON / ' + metadata['updated'])
        canv.drawRightString(WIDTH - 40, 25, f'{document.page:02d}')
        canv.restoreState()

    frame = Frame(MARGIN, 60, CONTENT_WIDTH, HEIGHT - 144,
                  leftPadding=0, rightPadding=0, topPadding=0, bottomPadding=0)
    doc.addPageTemplates(PageTemplate(id='paper', frames=[frame], onPage=page_frame))
    doc.build(markdown_story(body, source, metadata['lang']))
    return doc.page
