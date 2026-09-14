#!/usr/bin/env python3
"""Strict local-asset slideshow/video assembly; Python standard library only."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


class ProductionError(Exception):
    pass


def run(args):
    result = subprocess.run(args, capture_output=True, text=True, check=False)
    if result.returncode:
        raise ProductionError(f"{args[0]} failed: {result.stderr[-4000:]}")
    return result.stdout


def dependencies():
    missing = [name for name in ('ffmpeg', 'ffprobe') if not shutil.which(name)]
    if missing:
        raise ProductionError('Missing required dependencies: ' + ', '.join(missing))


def probe(path, count_frames=False):
    return json.loads(run(['ffprobe', '-v', 'error', '-protocol_whitelist', 'file',
                           '-show_format', '-show_streams', '-of', 'json']
                          + (['-count_frames'] if count_frames else []) + [str(path)]))


def keys(value, required, optional=()):
    if not isinstance(value, dict) or set(value) - set(required) - set(optional) or set(required) - set(value):
        raise ProductionError(f'Expected keys {required}, optional {optional}')


def positive(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
        raise ProductionError('Duration must be finite and positive')
    return float(value)


def asset(base, name, allowed):
    if not isinstance(name, str) or '://' in name:
        raise ProductionError('Assets must be local file paths')
    path = (base / name).resolve()
    if not path.is_file() or path.suffix.lower() not in allowed:
        raise ProductionError(f'Missing or unsupported asset: {path}')
    return path


def digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            checksum.update(chunk)
    return checksum.hexdigest()


def validate(manifest):
    data = json.loads(manifest.read_text(encoding='utf-8'))
    keys(data, ('clips',), ('audio', 'subtitles'))
    if not isinstance(data['clips'], list) or not data['clips']:
        raise ProductionError('clips must be a nonempty list')
    clips, sources = [], []
    for clip in data['clips']:
        keys(clip, ('path', 'kind', 'duration'))
        if clip['kind'] not in ('image', 'video'):
            raise ProductionError('kind must be image or video')
        path = asset(manifest.parent, clip['path'], ('.png', '.jpg', '.jpeg') if clip['kind'] == 'image' else ('.mp4', '.mov'))
        # Quantize once so validation, segment renders and muxing share a timeline.
        duration = max(1, math.floor(positive(clip['duration']) * 25 + .5)) / 25
        info = probe(path)
        if not any(s['codec_type'] == 'video' for s in info['streams']):
            raise ProductionError(f'No video stream: {path}')
        if clip['kind'] == 'video' and duration > float(info['format'].get('duration', 0)) + .04:
            raise ProductionError(f'Clip duration exceeds source: {path}')
        clips.append((path, clip['kind'], duration))
        sources.append(path)
    total = sum(c[2] for c in clips)
    audio = subtitles = None
    if 'audio' in data:
        audio = asset(manifest.parent, data['audio'], ('.wav', '.mp3', '.m4a'))
        info = probe(audio)
        if not any(s['codec_type'] == 'audio' for s in info['streams']) or float(info['format'].get('duration', 0)) + .04 < total:
            raise ProductionError('Audio must contain an audio stream and cover the timeline')
        sources.append(audio)
    if 'subtitles' in data:
        subtitles = asset(manifest.parent, data['subtitles'], ('.srt',))
        # Decode and require ordinary SRT rather than accepting arbitrary text.
        import re
        blocks = re.split(r'\n\s*\n', subtitles.read_text(encoding='utf-8-sig').strip())
        last = 0
        for block in blocks:
            lines = block.splitlines()
            if len(lines) < 3 or not lines[0].isdigit():
                raise ProductionError('Invalid SRT cue')
            match = re.fullmatch(r'(\d{2}):([0-5]\d):([0-5]\d),(\d{3}) --> (\d{2}):([0-5]\d):([0-5]\d),(\d{3})', lines[1])
            if not match:
                raise ProductionError('Unsupported SRT timing')
            v = list(map(int, match.groups()))
            start, end = [v[i]*3600 + v[i+1]*60 + v[i+2] + v[i+3]/1000 for i in (0, 4)]
            if start < last or end <= start or end > total:
                raise ProductionError('SRT cues must be ordered, nonoverlapping and within the timeline')
            last = end
        sources.append(subtitles)
    return data, clips, audio, subtitles, sources, total


def produce(manifest, output, mode):
    dependencies()
    manifest = manifest.resolve()
    data, clips, audio, subtitles, sources, total = validate(manifest)
    if output.exists():
        raise ProductionError('Output directory must not exist (source/output overwrite prohibited)')
    width, height = (640, 360) if mode == 'preview' else (1280, 720)
    provenance = [{'path': str(p), 'sha256': digest(p)} for p in dict.fromkeys([manifest] + sources)]
    output.parent.mkdir(parents=True, exist_ok=True)
    # All intermediate artifacts are cleaned on success and failure.
    with tempfile.TemporaryDirectory(prefix='.video-production-', dir=output.parent) as tmp:
        work = Path(tmp)
        for i, (path, kind, duration) in enumerate(clips):
            args = ['ffmpeg', '-v', 'error', '-nostdin', '-n', '-protocol_whitelist', 'file']
            if kind == 'image':
                args += ['-loop', '1']
            args += ['-i', str(path), '-frames:v', str(round(duration * 25)), '-map', '0:v:0', '-an',
                     '-vf', f'scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=25',
                     # No frame reordering: even one-frame segments share PTS=DTS
                     # and concatenate without overlapping decode timestamps.
                     '-c:v', 'libx264', '-bf', '0', '-preset', 'fast', '-pix_fmt', 'yuv420p', str(work / f'{i}.mp4')]
            run(args)
        (work / 'clips.txt').write_text(''.join(f"file '{i}.mp4'\n" for i in range(len(clips))))
        args = ['ffmpeg', '-v', 'error', '-nostdin', '-n', '-f', 'concat', '-safe', '1',
                '-protocol_whitelist', 'file', '-i', str(work / 'clips.txt')]
        for path in (audio, subtitles):
            if path:
                args += ['-protocol_whitelist', 'file', '-i', str(path)]
        args += ['-map', '0:v:0', '-c:v', 'copy']
        if audio:
            args += ['-map', '1:a:0', '-c:a', 'aac']
        if subtitles:
            args += ['-map', f'{2 if audio else 1}:s:0', '-c:s', 'mov_text']
        video = work / f'{mode}.mp4'
        args += ['-t', str(total), '-movflags', '+faststart', str(video)]
        run(args)
        info = probe(video, count_frames=True)
        streams = info['streams']
        actual = float(info['format']['duration'])
        if abs(actual - total) > .04:
            raise ProductionError(f'QC duration mismatch: {actual} vs {total}')
        visual = next(s for s in streams if s['codec_type'] == 'video')
        expected_frames = sum(round(c[2] * 25) for c in clips)
        if int(visual.get('nb_read_frames', 0)) != expected_frames or abs(float(visual.get('duration', 0)) - total) > .000001:
            raise ProductionError('QC video frame count/duration mismatch')
        if (visual['width'], visual['height'], visual['pix_fmt']) != (width, height, 'yuv420p'):
            raise ProductionError('QC video format mismatch')
        if bool(audio) != any(s['codec_type'] == 'audio' for s in streams) or bool(subtitles) != any(s['codec_type'] == 'subtitle' for s in streams):
            raise ProductionError('QC missing/unexpected streams')
        run(['ffmpeg', '-v', 'error', '-xerror', '-nostdin', '-i', str(video), '-map', '0:v', '-map', '0:a?', '-f', 'null', '-'])
        report = {'mode': mode, 'manifest': data, 'sources': provenance,
                  'timeline': {'fps': 25, 'clip_durations': [c[2] for c in clips], 'duration': total},
                  'output_sha256': digest(video), 'probe': info, 'full_decode': 'passed',
                  'tools': {n: run([n, '-version']).splitlines()[0] for n in ('ffmpeg', 'ffprobe')},
                  'limitations': ['Visual/editorial QC requires human review', 'Clip audio discarded; optional audio replaces it', 'Subtitles are selectable, not burned in']}
        delivery = work / 'delivery'
        delivery.mkdir()
        shutil.copyfile(video, delivery / video.name)
        (delivery / 'report.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        # Reserve exclusively, then atomically replace our empty reservation.
        # A failed staging write never creates a final delivery directory.
        output.mkdir()
        try:
            delivery.replace(output)
        except OSError:
            output.rmdir()
            raise
    return output / f'{mode}.mp4'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--mode', choices=('preview', 'final'), default='preview')
    args = parser.parse_args()
    try:
        print(produce(args.manifest, args.output, args.mode))
    except (ProductionError, OSError, ValueError, KeyError) as exc:
        print(f'error: {exc}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
