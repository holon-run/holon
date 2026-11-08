#!/usr/bin/env python3
"""Credential-free tests for the Android distribution artifact gate."""

import importlib.util
from pathlib import Path
import os
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile


ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("channels", ROOT / "verify-android-release-channels.py")
channels = importlib.util.module_from_spec(spec)
spec.loader.exec_module(channels)


def manifest(channel, installer=False):
    permission = f'''
  E: uses-permission (line=2)
    A: android:name(0x01010003)="{channels.INSTALL_PERMISSION}"''' if installer else ""
    return f'''E: manifest (line=1){permission}
  E: application (line=3)
    E: meta-data (line=4)
      A: android:name(0x01010003)="{channels.CHANNEL_KEY}"
      A: android:value(0x01010024)="{channel}"
'''


class SourceSetChannelTests(unittest.TestCase):
    source_root = ROOT.parent / "apps/android/app/src"
    android = "{http://schemas.android.com/apk/res/android}"

    def test_installer_permission_is_declared_only_in_direct(self):
        for source_set in ("main", "direct", "play"):
            with self.subTest(source_set=source_set):
                manifest = ET.parse(self.source_root / source_set / "AndroidManifest.xml")
                permissions = {
                    permission.get(self.android + "name")
                    for permission in manifest.findall("uses-permission")
                }
                self.assertEqual(channels.INSTALL_PERMISSION in permissions, source_set == "direct")

    def test_update_file_path_is_direct_only(self):
        common = {
            ("cache-path", "camera", "camera/"),
            ("cache-path", "shared-artifacts", "shared-artifacts/"),
            ("cache-path", "trace-exports", "trace-exports/"),
        }
        for source_set in ("main", "direct", "play"):
            with self.subTest(source_set=source_set):
                resource = self.source_root / source_set / "res/xml/file_paths.xml"
                if not resource.exists():
                    resource = self.source_root / "main/res/xml/file_paths.xml"
                paths = {
                    (path.tag, path.get("name"), path.get("path"))
                    for path in ET.parse(resource).getroot()
                }
                expected = common | {("cache-path", "updates", "updates/")} if source_set == "direct" else common
                self.assertEqual(paths, expected)

    def test_shared_provider_keeps_files_authority(self):
        manifest = ET.parse(self.source_root / "main/AndroidManifest.xml")
        provider = next(
            provider for provider in manifest.findall("application/provider")
            if provider.get(self.android + "name") == "androidx.core.content.FileProvider"
        )
        self.assertEqual(provider.get(self.android + "authorities"), "${applicationId}.files")
        self.assertEqual(provider.get(self.android + "exported"), "false")
        self.assertEqual(provider.get(self.android + "grantUriPermissions"), "true")


class ChannelTests(unittest.TestCase):
    def test_expected_channels(self):
        channels.check_manifest(manifest("direct", True), "direct")
        channels.check_manifest(manifest("play"), "play")

    def test_installer_permission_is_direct_only(self):
        for channel, installer in [("direct", False), ("play", True)]:
            with self.subTest(channel=channel), self.assertRaisesRegex(ValueError, "installer permission"):
                channels.check_manifest(manifest(channel, installer), channel)

    def test_channel_mismatch_and_missing_marker(self):
        for tree in [manifest("direct", True), "", manifest("play") + manifest("play")]:
            with self.subTest(tree=tree), self.assertRaisesRegex(ValueError, "distribution marker"):
                channels.check_manifest(tree, "play")

    def test_package_version_and_debuggable(self):
        good = "package: name='run.holon.android' versionCode='50001' versionName='0.50.1'"
        self.assertEqual(channels.package_version(good), ("50001", "0.50.1"))
        for bad in [good.replace("run.holon.android", "wrong.package"), good + "\napplication-debuggable", ""]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                channels.package_version(bad)

    def test_bundle_base_is_derived_from_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle, output = Path(directory) / "app.aab", Path(directory) / "base.apk"
            with zipfile.ZipFile(bundle, "w") as archive:
                archive.writestr("base/manifest/AndroidManifest.xml", b"play manifest")
                archive.writestr("base/resources.pb", b"resources")
                archive.writestr("base/res/xml/example.xml", b"resource")
                archive.writestr("base/dex/classes.dex", b"not needed")
            channels.bundle_base(bundle, output)
            with zipfile.ZipFile(output) as archive:
                self.assertEqual(archive.read("AndroidManifest.xml"), b"play manifest")
                self.assertEqual(set(archive.namelist()), {
                    "AndroidManifest.xml", "resources.pb", "res/xml/example.xml",
                })

    def test_bundle_manifest_required(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle, output = Path(directory) / "app.aab", Path(directory) / "base.apk"
            with zipfile.ZipFile(bundle, "w"):
                pass
            with self.assertRaisesRegex(ValueError, "base manifest missing"):
                channels.bundle_base(bundle, output)


SDK = Path(os.environ.get("ANDROID_HOME", ""))
AAPT2 = Path(os.environ.get("HOLON_ANDROID_AAPT2", str(SDK / "build-tools/36.0.0/aapt2")))
ANDROID_JAR = SDK / "platforms/android-36/android.jar"


@unittest.skipUnless(AAPT2.is_file() and ANDROID_JAR.is_file(), "Android SDK build tools/platform unavailable")
class AaptChannelTests(unittest.TestCase):
    """Exercise real binary/proto manifest decoding without Gradle or signing."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="holon-channel-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def build(self, channel, installer=False, package=channels.PACKAGE):
        manifest_path = self.root / f"{channel}.xml"
        permission = f'<uses-permission android:name="{channels.INSTALL_PERMISSION}" />' if installer else ""
        manifest_path.write_text(f'''<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="{package}" android:versionCode="50001" android:versionName="0.50.1">
    <uses-sdk android:minSdkVersion="26" android:targetSdkVersion="36" />
    {permission}
    <application android:label="Holon">
        <meta-data android:name="{channels.CHANNEL_KEY}" android:value="{channel}" />
    </application>
</manifest>''')
        apk = self.root / f"{channel}.apk"
        subprocess.run([
            str(AAPT2), "link", "--manifest", str(manifest_path), "-I", str(ANDROID_JAR),
            "-o", str(apk), *(["--proto-format"] if channel == "play" else []),
        ], check=True, capture_output=True)
        if channel != "play":
            return apk
        bundle = self.root / "play.aab"
        with zipfile.ZipFile(apk) as source, zipfile.ZipFile(bundle, "w") as target:
            for entry in source.infolist():
                name = "base/manifest/AndroidManifest.xml" if entry.filename == "AndroidManifest.xml" else "base/" + entry.filename
                target.writestr(name, source.read(entry))
        return bundle

    def test_real_direct_apk_and_play_bundle(self):
        channels.verify(self.build("direct", True), self.build("play"), AAPT2)

    def test_real_play_installer_permission_rejected(self):
        with self.assertRaisesRegex(ValueError, "installer permission"):
            channels.verify(self.build("direct", True), self.build("play", True), AAPT2)

    def test_real_play_package_mismatch_rejected(self):
        with self.assertRaisesRegex(ValueError, "package identity"):
            channels.verify(self.build("direct", True), self.build("play", package="wrong.package"), AAPT2)


if __name__ == "__main__":
    unittest.main()
