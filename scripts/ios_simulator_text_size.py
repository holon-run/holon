"""Verified system appearance/text-size lifecycle for dedicated UI simulators."""
from contextlib import contextmanager
import http.server
import json
import secrets
import subprocess
import time
import threading


MAXIMUM_TEXT_SIZE = "accessibility-extra-extra-extra-large"
# Case preparation/restoration runs outside XCTest's interactive deadlines.
_CONFIGURATION_TIMEOUT = 120
# The simctl ui service can hang transiently mid-run; runtime control calls
# retry within a bounded budget instead of failing the whole job on one miss.
_RUNTIME_RETRY_ATTEMPTS = 3
# XCTest's HTTP request times out at 40s; setting, readback and backoff share
# this smaller budget rather than each command owning a fresh retry budget.
_RUNTIME_REQUEST_TIMEOUT = 35
_RETRY_BACKOFF_SECONDS = (1.0, 4.0)
_TEXT_SIZES = {
    "extra-small", "small", "medium", "large", "extra-large",
    "extra-extra-large", "extra-extra-extra-large", "accessibility-medium",
    "accessibility-large", "accessibility-extra-large",
    "accessibility-extra-extra-large", MAXIMUM_TEXT_SIZE,
}


def _read_appearance(simulator, *, timeout=15):
    result = subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "appearance"],
        check=True, capture_output=True, text=True, timeout=timeout,
    )
    appearance = result.stdout.strip()
    if appearance not in ("light", "dark"):
        raise RuntimeError(f"无法读取可恢复的模拟器系统外观：{appearance!r}")
    return appearance


def _set_appearance(simulator, appearance, *, timeout=15):
    if appearance not in ("light", "dark"):
        raise ValueError("不支持的系统外观")
    subprocess.run(
        ["xcrun", "simctl", "ui", simulator, "appearance", appearance],
        check=True, capture_output=True, text=True, timeout=timeout,
    )
    actual = _read_appearance(simulator, timeout=timeout)
    print(f"模拟器 {simulator} 系统外观：{actual}", flush=True)
    if actual != appearance:
        raise RuntimeError(f"系统外观核验失败：期望 {appearance}，实际 {actual}")


@contextmanager
def simulator_appearance(simulator, appearance):
    original = _read_appearance(simulator, timeout=_CONFIGURATION_TIMEOUT)
    try:
        _set_appearance(simulator, appearance, timeout=_CONFIGURATION_TIMEOUT)
        yield appearance
    finally:
        _set_appearance(simulator, original, timeout=_CONFIGURATION_TIMEOUT)


def _run_content_size(simulator, arguments, *, timeout, deadline=None):
    command = ["xcrun", "simctl", "ui", simulator, "content_size", *arguments]
    if deadline is not None:
        timeout = min(timeout, deadline - time.monotonic())
        if timeout <= 0:
            raise subprocess.TimeoutExpired(command, 0)
    return subprocess.run(
        command, check=True, capture_output=True, text=True, timeout=timeout,
    )


def _read_text_size(simulator, *, timeout=15, deadline=None):
    result = _run_content_size(simulator, [], timeout=timeout, deadline=deadline)
    category = result.stdout.strip()
    if category not in _TEXT_SIZES:
        raise RuntimeError(f"无法读取可恢复的模拟器系统字号：{category!r}")
    return category


def set_simulator_text_size(simulator, category, phase="运行时切换", *, timeout=15,
                            attempts=1, deadline=None):
    if category not in _TEXT_SIZES:
        raise ValueError("不支持的系统字号")
    needs_set = True
    uncertain_set = False
    for attempt in range(1, attempts + 1):
        try:
            if needs_set:
                try:
                    _run_content_size(simulator, [category], timeout=timeout, deadline=deadline)
                except subprocess.TimeoutExpired:
                    if attempts == 1:
                        raise
                    # Killing simctl does not establish whether its service applied
                    # the setting. Verify before sending another mutation.
                    uncertain_set = True
                    print(f"模拟器 {simulator}：字号设置命令超时，先核验实际系统字号", flush=True)
                else:
                    uncertain_set = False
                needs_set = False
            actual = _read_text_size(simulator, timeout=timeout, deadline=deadline)
            print(f"模拟器 {simulator} 系统字号（{phase}）：{actual}", flush=True)
            if actual == category:
                return actual
            failure = RuntimeError(f"系统字号核验失败：期望 {category}，实际 {actual}")
            if not uncertain_set:
                raise failure
            needs_set = True
        except subprocess.SubprocessError as error:
            failure = error
        if attempt == attempts:
            raise failure
        backoff = _RETRY_BACKOFF_SECONDS[min(attempt - 1, len(_RETRY_BACKOFF_SECONDS) - 1)]
        if deadline is not None and time.monotonic() + backoff >= deadline:
            raise failure
        print(f"模拟器 {simulator}：simctl ui content_size 第 {attempt}/{attempts} "
              f"次尝试失败（{type(failure).__name__}），{backoff:g}s 后重试", flush=True)
        time.sleep(backoff)


def initialize_simulator_text_size(simulator):
    """Establish a verified baseline on the harness-owned fresh simulator."""
    subprocess.run(
        ["xcrun", "simctl", "bootstatus", simulator, "-b"],
        check=True, timeout=180,
    )
    # Boot completion does not establish the simctl UI service connection.
    # Setup keeps a bounded configuration budget; runtime changes retain 15s.
    print(f"模拟器 {simulator}：等待系统字号服务就绪", flush=True)
    _read_text_size(simulator, timeout=_CONFIGURATION_TIMEOUT)
    set_simulator_text_size(simulator, "large", "专用测试基线初始化",
                            timeout=_CONFIGURATION_TIMEOUT)


@contextmanager
def simulator_text_size(simulator, category):
    original = _read_text_size(simulator, timeout=_CONFIGURATION_TIMEOUT)
    print(f"模拟器 {simulator} 原系统字号：{original}", flush=True)
    try:
        set_simulator_text_size(simulator, category, "设置", timeout=_CONFIGURATION_TIMEOUT)
        yield category
    finally:
        # Also restore if configuration, XCTest or its runtime size change fails.
        set_simulator_text_size(simulator, original, "恢复", timeout=_CONFIGURATION_TIMEOUT)


@contextmanager
def runtime_text_size_control(simulator):
    """Let XCTest change verified system preferences without relaunching the app."""
    token = secrets.token_hex(32)

    class Control(http.server.BaseHTTPRequestHandler):
        timeout = 5

        def log_message(self, *args):
            pass

        def reply(self, status, category=None):
            try:
                if status != 200:
                    self.send_error(
                        status, "System text-size verification failed" if status == 500 else None)
                    return
                body = json.dumps({"category": category}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                # Client cancellation must not interrupt draining the operation
                # or obscure a failed verification with a response-write traceback.
                print(f"模拟器 {simulator}：字号控制响应 {status} 未送达，客户端已断开", flush=True)

        def do_POST(self):
            deadline = time.monotonic() + _RUNTIME_REQUEST_TIMEOUT
            if self.path != "/content-size" or not secrets.compare_digest(
                self.headers.get("Authorization", "").encode(), ("Bearer " + token).encode()
            ):
                self.reply(403)
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
                self.reply(400)
                return
            try:
                actual = set_simulator_text_size(
                    simulator, category, attempts=_RUNTIME_RETRY_ATTEMPTS, deadline=deadline)
            except (subprocess.SubprocessError, RuntimeError) as error:
                print(f"模拟器 {simulator}：系统字号切换失败（{type(error).__name__}）：{error}",
                      flush=True)
                self.reply(500)
                return
            self.reply(200, actual)

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
