"""Verified system appearance/text-size lifecycle for dedicated UI simulators."""
from contextlib import contextmanager
import http.server
import json
import secrets
import subprocess
import threading


MAXIMUM_TEXT_SIZE = "accessibility-extra-extra-extra-large"
_TEXT_SIZES = {
    "extra-small", "small", "medium", "large", "extra-large",
    "extra-extra-large", "extra-extra-extra-large", "accessibility-medium",
    "accessibility-large", "accessibility-extra-large",
    "accessibility-extra-extra-large", MAXIMUM_TEXT_SIZE,
}


def _read_appearance(simulator):
    result = subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "appearance"],
        check=True, capture_output=True, text=True, timeout=15,
    )
    appearance = result.stdout.strip()
    if appearance not in ("light", "dark"):
        raise RuntimeError(f"无法读取可恢复的模拟器系统外观：{appearance!r}")
    return appearance


def _set_appearance(simulator, appearance):
    if appearance not in ("light", "dark"):
        raise ValueError("不支持的系统外观")
    subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "appearance", appearance],
        check=True, capture_output=True, text=True, timeout=15,
    )
    actual = _read_appearance(simulator)
    print(f"模拟器 {simulator} 系统外观：{actual}", flush=True)
    if actual != appearance:
        raise RuntimeError(f"系统外观核验失败：期望 {appearance}，实际 {actual}")


@contextmanager
def simulator_appearance(simulator, appearance):
    original = _read_appearance(simulator)
    try:
        _set_appearance(simulator, appearance)
        yield appearance
    finally:
        _set_appearance(simulator, original)


def _read_text_size(simulator, *, timeout=15):
    result = subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "content_size"],
        check=True, capture_output=True, text=True, timeout=timeout,
    )
    category = result.stdout.strip()
    if category not in _TEXT_SIZES:
        raise RuntimeError(f"无法读取可恢复的模拟器系统字号：{category!r}")
    return category


def set_simulator_text_size(simulator, category, phase="运行时切换"):
    if category not in _TEXT_SIZES:
        raise ValueError("不支持的系统字号")
    subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "content_size", category],
        check=True, capture_output=True, text=True, timeout=15,
    )
    actual = _read_text_size(simulator)
    print(f"模拟器 {simulator} 系统字号（{phase}）：{actual}", flush=True)
    if actual != category:
        raise RuntimeError(f"系统字号核验失败：期望 {category}，实际 {actual}")
    return actual


def initialize_simulator_text_size(simulator):
    """Establish a verified baseline on the harness-owned fresh simulator."""
    subprocess.run(
        ["xcrun", "simctl", "bootstatus", simulator, "-b"],
        check=True, timeout=180,
    )
    # Boot completion does not establish the first simctl UI service connection.
    # Give cold startup its own budget; runtime changes keep their 15s deadline.
    print(f"模拟器 {simulator}：等待系统字号服务就绪", flush=True)
    _read_text_size(simulator, timeout=120)
    set_simulator_text_size(simulator, "large", "专用测试基线初始化")


@contextmanager
def simulator_text_size(simulator, category):
    original = _read_text_size(simulator)
    print(f"模拟器 {simulator} 原系统字号：{original}", flush=True)
    try:
        set_simulator_text_size(simulator, category, "设置")
        yield category
    finally:
        # Also restore if configuration, XCTest or its runtime size change fails.
        set_simulator_text_size(simulator, original, "恢复")


@contextmanager
def runtime_text_size_control(simulator):
    """Let XCTest change verified system preferences without relaunching the app."""
    token = secrets.token_hex(32)

    class Control(http.server.BaseHTTPRequestHandler):
        timeout = 5

        def log_message(self, *args):
            pass

        def do_POST(self):
            if self.path != "/content-size" or not secrets.compare_digest(
                self.headers.get("Authorization", "").encode(), ("Bearer " + token).encode()
            ):
                self.send_error(403)
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 1024:
                    raise ValueError()
                request = json.loads(self.rfile.read(length))
                if not isinstance(request, dict) or set(request) != {"category"}:
                    raise ValueError()
                category = request["category"]
                if category not in ("large", MAXIMUM_TEXT_SIZE):
                    raise ValueError()
            except (ValueError, TypeError):
                self.send_error(400)
                return
            try:
                actual = set_simulator_text_size(simulator, category)
            except (subprocess.SubprocessError, RuntimeError):
                self.send_error(500, "System text-size verification failed")
                return
            body = json.dumps({"category": actual}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    # One request at a time; shutdown waits for the active setter/readback.
    server = http.server.HTTPServer(("127.0.0.1", 0), Control)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/content-size", token
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
