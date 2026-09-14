"""Real, offline FFmpeg fixtures; missing binaries fail rather than skip CI."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('produce', Path(__file__).parents[1] / 'scripts/produce.py')
p = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(p)


class ProductionTests(unittest.TestCase):
    def setUp(self):
        p.dependencies()
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest = self.root / 'manifest.json'
        self.data = {'clips': [{'path': 'still.png', 'kind': 'image', 'duration': .4},
                               {'path': 'clip.mp4', 'kind': 'video', 'duration': .4}],
                     'audio': 'sound.wav', 'subtitles': 'captions.srt'}
        for name, source, extra in [
            ('still.png', 'color=c=blue:s=160x90', ['-frames:v', '1', '-threads', '1']),
            ('clip.mp4', 'testsrc2=s=160x90:r=25:d=1', ['-c:v', 'libx264']),
            ('sound.wav', 'sine=frequency=440:duration=1', [])
        ]:
            p.run(['ffmpeg', '-v', 'error', '-nostdin', '-f', 'lavfi', '-i', source] + extra + [str(self.root / name)])
        (self.root / 'captions.srt').write_text('1\n00:00:00,000 --> 00:00:00,700\nLocal fixture\n')
        self.write()

    def write(self):
        self.manifest.write_text(json.dumps(self.data))

    def test_preview_and_final_with_provenance_and_decode(self):
        original = {name: p.digest(self.root / name) for name in ('still.png', 'clip.mp4', 'sound.wav', 'captions.srt')}
        for mode, size in [('preview', (640, 360)), ('final', (1280, 720))]:
            out = self.root / mode
            video = p.produce(self.manifest, out, mode)
            report = json.loads((out / 'report.json').read_text())
            self.assertEqual(report['full_decode'], 'passed')
            self.assertEqual(report['output_sha256'], p.digest(video))
            streams = p.probe(video)['streams']
            self.assertEqual({s['codec_type'] for s in streams}, {'video', 'audio', 'subtitle'})
            self.assertEqual((streams[0]['width'], streams[0]['height']), size)
            p.run(['ffmpeg', '-v', 'error', '-xerror', '-i', str(video), '-f', 'null', '-'])
            with self.assertRaisesRegex(p.ProductionError, 'must not exist'):
                p.produce(self.manifest, out, mode)
        self.assertEqual(original, {name: p.digest(self.root / name) for name in original})
        self.assertFalse(list(self.root.glob('.video-production-*')))

    def test_optional_tracks(self):
        del self.data['audio'], self.data['subtitles']
        self.write()
        video = p.produce(self.manifest, self.root / 'silent', 'preview')
        self.assertEqual([s['codec_type'] for s in p.probe(video)['streams']], ['video'])

    def test_frame_quantized_timeline(self):
        del self.data['audio'], self.data['subtitles']
        for kind, name in [('image', 'still.png'), ('video', 'clip.mp4')]:
            with self.subTest(kind=kind):
                self.data['clips'] = [{'path': name, 'kind': kind, 'duration': duration}
                                      for duration in (.01, .42, .42)]
                self.write()
                video = p.produce(self.manifest, self.root / f'quantized-{kind}', 'preview')
                report = json.loads((video.parent / 'report.json').read_text())
                self.assertEqual(report['timeline']['clip_durations'], [.04, .44, .44])
                self.assertAlmostEqual(report['timeline']['duration'], .92)
                info = p.probe(video, count_frames=True)
                visual = info['streams'][0]
                self.assertEqual(int(visual['nb_read_frames']), 23)
                self.assertAlmostEqual(float(visual['duration']), .92, places=6)
                self.assertAlmostEqual(float(info['format']['duration']), .92, places=2)

    def test_qc_rejects_missing_frame(self):
        probe = p.probe

        def missing_frame(path, count_frames=False):
            info = probe(path, count_frames=count_frames)
            if count_frames:
                visual = next(s for s in info['streams'] if s['codec_type'] == 'video')
                visual['nb_read_frames'] = str(int(visual['nb_read_frames']) - 1)
            return info

        out = self.root / 'bad-qc'
        with patch.object(p, 'probe', side_effect=missing_frame):
            with self.assertRaisesRegex(p.ProductionError, 'frame count'):
                p.produce(self.manifest, out, 'preview')
        self.assertFalse(out.exists())

    def test_failed_delivery_can_be_retried(self):
        write_text = Path.write_text

        def fail_report(path, *args, **kwargs):
            if path.name == 'report.json':
                raise OSError('simulated report write failure')
            return write_text(path, *args, **kwargs)

        out = self.root / 'retry'
        with patch.object(Path, 'write_text', new=fail_report):
            with self.assertRaisesRegex(OSError, 'report write failure'):
                p.produce(self.manifest, out, 'preview')
        self.assertFalse(out.exists())
        self.assertFalse(list(self.root.glob('.video-production-*')))
        with patch.object(Path, 'replace', side_effect=OSError('simulated rename failure')):
            with self.assertRaisesRegex(OSError, 'rename failure'):
                p.produce(self.manifest, out, 'preview')
        self.assertFalse(out.exists())
        self.assertFalse(list(self.root.glob('.video-production-*')))
        p.produce(self.manifest, out, 'preview')
        self.assertTrue((out / 'preview.mp4').is_file())
        self.assertTrue((out / 'report.json').is_file())

    def test_reject_bad_inputs(self):
        baseline = json.loads(json.dumps(self.data))
        mutations = [lambda d: d.update(unknown=True),
                     lambda d: d.update(clips=[]),
                     lambda d: d['clips'][0].update(duration=-1),
                     lambda d: d['clips'][0].update(duration=True),
                     lambda d: d['clips'][0].update(path='https://example.com/a.png'),
                     lambda d: d['clips'][0].update(path='missing.png'),
                     lambda d: d['clips'][1].update(duration=20),
                     lambda d: d['clips'][0].update(kind='animation'),
                     lambda d: d.update(audio='still.png')]
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                self.data = json.loads(json.dumps(baseline))
                mutate(self.data)
                self.write()
                with self.assertRaises(p.ProductionError):
                    p.produce(self.manifest, self.root / 'bad', 'preview')
                self.assertFalse((self.root / 'bad').exists())
        self.data = baseline
        self.write()
        (self.root / 'captions.srt').write_text('not subtitles')
        with self.assertRaisesRegex(p.ProductionError, 'SRT'):
            p.validate(self.manifest)
        del self.data['subtitles']
        self.write()
        (self.root / 'clip.mp4').write_bytes(b'corrupt media')
        with self.assertRaises(p.ProductionError):
            p.produce(self.manifest, self.root / 'bad', 'preview')

    def test_missing_dependencies(self):
        for missing in ('ffmpeg', 'ffprobe'):
            with patch.object(p.shutil, 'which', side_effect=lambda n: None if n == missing else '/fake/' + n):
                with self.assertRaisesRegex(p.ProductionError, missing):
                    p.produce(self.manifest, self.root / 'missing', 'preview')
        self.assertFalse((self.root / 'missing').exists())


if __name__ == '__main__':
    unittest.main()
