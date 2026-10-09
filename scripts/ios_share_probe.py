"""Build/install only the isolated native sender on the harness-owned simulator."""
import pathlib
import plistlib
import subprocess
import platform

BUNDLE_ID = "run.holon.ios.share-probe"


def install(simulator, repo, directory):
    app = pathlib.Path(directory) / "HolonShareProbe.app"
    app.mkdir()
    sdk = subprocess.check_output(
        ["xcrun", "--sdk", "iphonesimulator", "--show-sdk-path"], text=True).strip()
    subprocess.run(["xcrun", "swiftc", "-parse-as-library", "-sdk", sdk,
                    "-target", platform.machine() + "-apple-ios18.0-simulator",
                    str(pathlib.Path(repo) / "scripts/ios-share-probe.swift"),
                    "-o", str(app / "HolonShareProbe")], check=True)
    info = {"CFBundleIdentifier": BUNDLE_ID, "CFBundleExecutable": "HolonShareProbe",
            "CFBundleName": "Holon Share Probe", "CFBundlePackageType": "APPL",
            "CFBundleVersion": "1", "CFBundleShortVersionString": "1.0",
            "MinimumOSVersion": "18.0", "LSRequiresIPhoneOS": True,
            "UIDeviceFamily": [1, 2], "UILaunchScreen": {}}
    (app / "Info.plist").write_bytes(plistlib.dumps(info))
    subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True)
    subprocess.run(["xcrun", "simctl", "install", simulator, str(app)], check=True)
