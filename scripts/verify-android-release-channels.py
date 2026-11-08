#!/usr/bin/env python3
"""Check distribution manifests and versions, not artifact signatures."""

import argparse
from pathlib import Path
import os
import re
import subprocess
import tempfile
import zipfile


INSTALL_PERMISSION = "android.permission.REQUEST_INSTALL_PACKAGES"
CHANNEL_KEY = "run.holon.android.UPDATE_CHANNEL"
PACKAGE = "run.holon.android"


def manifest_elements(tree):
    """Read string attributes from aapt2's decoded binary XML tree."""
    elements = []
    current = None
    for line in tree.splitlines():
        element = re.match(r"\s*E: ([\w-]+)", line)
        if element:
            current = (element[1], {})
            elements.append(current)
        attribute = re.match(r'\s*A: ([^\s(=]+)(?:\(0x[0-9a-f]+\))?="([^"]*)"', line)
        if attribute and current:
            name = attribute[1].replace("http://schemas.android.com/apk/res/android:", "android:")
            current[1][name] = attribute[2]
    return elements


def check_manifest(tree, channel):
    elements = manifest_elements(tree)
    channels = [
        attrs.get("android:value")
        for tag, attrs in elements
        if tag == "meta-data" and attrs.get("android:name") == CHANNEL_KEY
    ]
    if channels != [channel]:
        raise ValueError(f"Expected exactly one {channel} distribution marker")
    permissions = {
        attrs.get("android:name")
        for tag, attrs in elements
        if tag.startswith("uses-permission")
    }
    if (INSTALL_PERMISSION in permissions) != (channel == "direct"):
        raise ValueError(f"Unexpected installer permission for {channel}")


def package_version(badging):
    match = re.search(r"package: name='([^']+)' versionCode='(\d+)' versionName='([^']+)'", badging)
    if not match or match[1] != PACKAGE or "application-debuggable" in badging:
        raise ValueError("Incorrect package identity or debuggable release")
    return match[2], match[3]


def bundle_base(bundle, output):
    # An AAB base module contains a proto APK: put its manifest back at the
    # APK root so aapt2 can convert it to binary XML for the same checks.
    with zipfile.ZipFile(bundle) as source, zipfile.ZipFile(output, "w") as target:
        for entry in source.infolist():
            if entry.filename == "base/manifest/AndroidManifest.xml":
                target.writestr("AndroidManifest.xml", source.read(entry))
            elif entry.filename == "base/resources.pb" or entry.filename.startswith("base/res/"):
                target.writestr(entry.filename.removeprefix("base/"), source.read(entry))
    with zipfile.ZipFile(output) as result:
        if "AndroidManifest.xml" not in result.namelist():
            raise ValueError("AAB base manifest missing")


def verify(apk, bundle, aapt2):
    def dump(kind, path):
        args = [str(aapt2), "dump", kind, str(path)]
        if kind == "xmltree":
            args += ["--file", "AndroidManifest.xml"]
        return subprocess.check_output(args, text=True)

    check_manifest(dump("xmltree", apk), "direct")
    version = package_version(dump("badging", apk))
    with tempfile.TemporaryDirectory(prefix="holon-android-channel-") as directory:
        proto = Path(directory) / "base-proto.apk"
        binary = Path(directory) / "base.apk"
        bundle_base(bundle, proto)
        subprocess.run([
            str(aapt2), "convert", "--output-format", "binary",
            "-o", str(binary), str(proto),
        ], check=True)
        check_manifest(dump("xmltree", binary), "play")
        if package_version(dump("badging", binary)) != version:
            raise ValueError("Direct APK and Play AAB version mismatch")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apk", type=Path)
    parser.add_argument("aab", type=Path)
    parser.add_argument("--aapt2", type=Path, default=Path(os.environ.get("ANDROID_HOME", "")) / "build-tools/36.0.0/aapt2")
    args = parser.parse_args()
    try:
        verify(args.apk, args.aab, args.aapt2)
    except (ValueError, OSError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        parser.exit(1, f"Android channel verification failed: {error}\n")
    print("Verified direct APK / Play AAB channels, installer permissions and package/version (not signatures).")


if __name__ == "__main__":
    main()
