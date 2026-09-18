#!/usr/bin/env python3
"""Build a review PDF from the selected language's Markdown. Never publishes."""

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile

import reportlab

from layout import register_fonts, render_pdf

PAPER_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = PAPER_ROOT.parents[1]


def read_source(path, lang):
    value = path.read_text(encoding='utf-8')
    if not value.startswith('---\n'):
        raise ValueError('Expected a metadata header delimited by --- lines.')
    header, body = value[4:].split('\n---\n', 1)
    metadata = {}
    for line in header.splitlines():
        key, sep, val = line.partition(':')
        if not sep or key in metadata:
            raise ValueError(f'Invalid metadata line: {line}')
        metadata[key.strip()] = val.strip()
    for key in ['lang', 'version', 'updated', 'status', 'title']:
        if not metadata.get(key):
            raise ValueError(f'Missing metadata: {key}')
    if metadata['lang'] != lang:
        raise ValueError('Source language does not match --lang.')
    return metadata, body


def select_font(requested, bold=False):
    if requested:
        result = Path(requested).expanduser().resolve()
        if not result.is_file():
            raise ValueError(f'Font not found: {result}')
        return result
    name = 'Bold.ttf' if bold else 'Regular.ttf'
    candidates = [PAPER_ROOT / 'tools/fonts' / name]
    candidates.append(Path('/System/Library/Fonts') /
                      ('STHeiti Medium.ttc' if bold else 'STHeiti Light.ttc'))
    for path in candidates:
        if path.is_file():
            return path
    raise ValueError('No font found. Set --font-regular and --font-bold; see tools/fonts/README.md.')


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lang', choices=['zh-CN', 'en'], default='zh-CN')
    parser.add_argument('--output-dir', type=Path)
    parser.add_argument('--font-regular', default=os.environ.get('HOLON_PAPER_FONT_REGULAR'))
    parser.add_argument('--font-bold', default=os.environ.get('HOLON_PAPER_FONT_BOLD'))
    parser.add_argument('--preview', action='store_true', help='Render PNGs with pdftoppm.')
    args = parser.parse_args()
    source = PAPER_ROOT / f'{args.lang}.md'
    if not source.is_file():
        parser.error(f'No {args.lang} source yet: {source}')
    output_dir = (args.output_dir or REPO_ROOT / 'build/lite-paper' / args.lang).resolve()
    website = (REPO_ROOT / 'docs/website').resolve()
    if output_dir.is_relative_to(website):
        parser.error('Build into a review directory; copy an approved PDF to website assets separately.')
    converter = shutil.which('pdftoppm')
    if args.preview and not converter:
        parser.error('--preview requires pdftoppm on PATH.')
    try:
        metadata, body = read_source(source, args.lang)
        regular = select_font(args.font_regular)
        bold = select_font(args.font_bold, bold=True)
        register_fonts(regular, bold)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / 'holon-lite-paper.pdf'
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(suffix='.pdf', dir=output_dir, delete=False) as stream:
            temporary = Path(stream.name)
        count = render_pdf(body, source, metadata, temporary)
        temporary.replace(output)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    info = {
        'source': source.relative_to(REPO_ROOT).as_posix(),
        'source_sha256': sha256(source),
        'metadata': metadata,
        'reportlab_version': reportlab.Version,
        'pages': count,
        'fonts': {role: {'name': font.name, 'sha256': sha256(font)}
                  for role, font in [('regular', regular), ('bold', bold)]},
        'renderer_sha256': {name: sha256(PAPER_ROOT / 'tools' / name)
                           for name in ['build.py', 'layout.py']},
        'assets_sha256': {
            (source.parent / name).resolve().relative_to(REPO_ROOT).as_posix():
                sha256((source.parent / name).resolve())
            for name in re.findall(r'^!\[[^\]]*\]\(([^)]+)\)$', body, re.MULTILINE)
        },
        'pdf_sha256': sha256(output),
        'preview_rendered': False,
        'visual_review': 'pending',
    }
    info_path = output_dir / 'build-info.json'
    info_path.write_text(json.dumps(info, ensure_ascii=False, indent=2) + '\n')
    if args.preview:
        # Stage previews so a failed render cannot mix old and new page sets.
        with tempfile.TemporaryDirectory(prefix='previews-', dir=output_dir) as temp:
            subprocess.run([converter, '-scale-to', '1300', '-png', str(output),
                            str(Path(temp) / 'page')], check=True)
            previews = output_dir / 'previews'
            previews.mkdir(exist_ok=True)
            for stale in previews.glob('page-*.png'):
                stale.unlink()
            for rendered in Path(temp).glob('page-*.png'):
                rendered.replace(previews / rendered.name)
        info['preview_rendered'] = True
        info_path.write_text(json.dumps(info, ensure_ascii=False, indent=2) + '\n')
    print(f'{output} ({count} pages)')


if __name__ == '__main__':
    main()
