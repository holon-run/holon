#!/usr/bin/env python3
"""Verify the Android browser against a disposable scripted Holon daemon."""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import shutil


CHECK_SCRIPT = """
(async () => {
  window.verification = {};
  const context = await Holon.context();
  window.verification.context = context.session.authenticated &&
    context.agent_id === 'demo-android' && context.app_id === 'browser-check';
  const requestId = 'browser-check-' + crypto.randomUUID();
  const events = Holon.events(event => {
    if (event.event.request_id === requestId) {
      window.verification.event = true;
      events.close();
    }
  }, () => { window.verification.failed = true; });
  events.onopen = async () => {
    const response = await Holon.request('browser-verification', {fixture: true}, requestId);
    window.verification.request = response.ok && response.request_id === requestId;
  };
})().catch(() => { window.verification = {failed: true}; });
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True, help="Dedicated, disposable Android emulator")
    parser.add_argument("--artifacts", required=True, type=Path, help="New private evidence directory")
    parser.add_argument("--adb", default="/opt/android-sdk/platform-tools/adb")
    parser.add_argument("--holon", default="holon")
    parser.add_argument("--port", type=int, default=17983)
    parser.add_argument("--provider-port", type=int, default=17984)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    adb = [args.adb, "-s", args.serial]

    def device(*command, **kwargs):
        return subprocess.run(adb + list(command), check=True, **kwargs)

    if device("shell", "getprop", "ro.kernel.qemu", capture_output=True, text=True).stdout.strip() != "1":
        parser.error("Only an explicitly selected disposable emulator is permitted")
    apks = [
        root / "apps/android/app/build/outputs/apk/debug/app-debug.apk",
        root / "apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk",
    ]
    if not all(apk.is_file() for apk in apks):
        parser.error("Build :app:assembleDebug :app:assembleDebugAndroidTest first")
    args.artifacts.mkdir(parents=True, mode=0o700, exist_ok=False)
    # The runtime Unix socket needs a short path; long evidence paths can exceed SUN_LEN.
    runtime_directory = tempfile.TemporaryDirectory(prefix="holon-browser-")
    home = Path(runtime_directory.name) / "runtime"
    bootstrap_path = args.artifacts / "bootstrap"
    launch_path = args.artifacts / "daemon-launch.log"
    fixture = None
    reversed_port = False
    try:
        with launch_path.open("w") as launch:
            fixture = subprocess.Popen([
                sys.executable, str(root / "scripts/android-store-demo.py"),
                "--home", str(home), "--holon", args.holon,
                "--port", str(args.port), "--provider-port", str(args.provider_port),
                "--client-token-file", str(bootstrap_path),
            ], stdout=launch, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + 40
            while "Demo ready:" not in launch_path.read_text():
                if fixture.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("Disposable daemon failed to start; inspect daemon-launch.log")
                time.sleep(0.25)
            app = home / "agents/demo-android/apps/browser-check"
            app.mkdir(parents=True)
            (app / "manifest.json").write_text(json.dumps({
                "id": "browser-check", "name": "Browser verification", "version": "1.0.0", "entry": "index.html",
            }))
            (app / "index.html").write_text(
                '<!doctype html><title>Browser verification</title><script src="holon.js"></script><script src="check.js"></script>'
            )
            (app / "check.js").write_text(CHECK_SCRIPT)
            for apk in apks:
                device("install", "-r", str(apk))
            cold_start_passed = True
            for phase in ("seed", "check"):
                result = device(
                    "shell", "am", "instrument", "-w", "-r",
                    "-e", "runtimeBrowserColdStart", phase,
                    "-e", "class", "run.holon.android.app.RuntimeBrowserProfileIntegrationTest#coldStartDeletesAbandonedProfileWithoutLoadingIt",
                    "run.holon.android.test/androidx.test.runner.AndroidJUnitRunner",
                    text=True, capture_output=True, timeout=60,
                )
                (args.artifacts / f"cold-start-{phase}.log").write_text(result.stdout + result.stderr)
                if "OK (1 test)" not in result.stdout or "FAILURES!!!" in result.stdout:
                    cold_start_passed = False
                device("shell", "am", "force-stop", "run.holon.android")
            device("reverse", f"tcp:{args.port}", f"tcp:{args.port}")
            reversed_port = True
            # Bootstrap stays in private fixture/app files, never argv, page code or logs.
            configuration = json.dumps({
                "baseUrl": f"http://127.0.0.1:{args.port}/api/",
                "bootstrap": bootstrap_path.read_text(),
            })
            device("shell", "run-as run.holon.android sh -c "
                   "'mkdir -p files && cat > files/runtime-browser-fixture.json'",
                   input=configuration, text=True, capture_output=True)
            result = device(
                "shell", "am", "instrument", "-w", "-r",
                "-e", "runtimeBrowserDaemon", "true",
                "-e", "class", "run.holon.android.app.RuntimeBrowserDaemonIntegrationTest",
                "run.holon.android.test/androidx.test.runner.AndroidJUnitRunner",
                text=True, capture_output=True, timeout=180,
            )
            (args.artifacts / "instrumentation.log").write_text(result.stdout + result.stderr)
            if "OK (1 test)" not in result.stdout or any(
                marker in result.stdout for marker in ("FAILURES!!!", "INSTRUMENTATION_FAILED", "Process crashed")
            ):
                raise RuntimeError("Browser daemon verification failed; inspect instrumentation.log")
            if not cold_start_passed:
                raise RuntimeError("App SDK round trip passed but cold-start cleanup failed; inspect cold-start logs")
            print("PASS: native login → App SDK context/request/events; close preserves session; browser logout revokes it.")
    finally:
        if reversed_port:
            subprocess.run(adb + ["shell", "run-as", "run.holon.android", "rm", "-f", "files/runtime-browser-fixture.json"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            subprocess.run(adb + ["reverse", "--remove", f"tcp:{args.port}"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if fixture is not None and fixture.poll() is None:
            fixture.terminate()
            try:
                fixture.wait(timeout=15)
            except subprocess.TimeoutExpired:
                fixture.kill()
                fixture.wait()
        if (home / "daemon.log").is_file():
            shutil.copyfile(home / "daemon.log", args.artifacts / "daemon.log")
        bootstrap_path.unlink(missing_ok=True)
        runtime_directory.cleanup()


if __name__ == "__main__":
    main()
