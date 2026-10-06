#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ -n "${HOLON_CONTRACT_BIN:-}" ]]; then
  binary="$HOLON_CONTRACT_BIN"
else
  # Default verification must exercise this checkout, not a stale installed CLI.
  cargo build --bin holon
  binary="${CARGO_TARGET_DIR:-$PWD/target}/debug/holon"
fi
binary="$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$binary")"
# Python owns the temporary directory, daemon and actual HTTP prefix proxy.
# No caller provenance is removed or fabricated.
python3 - "$binary" "$PWD" <<'PY'
import http.client
import http.server
import json
import os
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

binary, repo = sys.argv[1:]
with tempfile.TemporaryDirectory(prefix="holon-ios-send-contract-") as temporary:
    root = pathlib.Path(temporary)
    # Do not inherit provider credentials, production paths or daemon settings.
    env = {k: v for k, v in os.environ.items()
           if k in ("PATH", "TMPDIR", "LANG", "SystemRoot")
           or k.startswith("HOLON_CALLER_")}
    env.update(HOME=str(root), HOLON_HOME=str(root / "holon"),
               HOLON_WORKSPACE_DIR=str(root), HOLON_IOS_TEST_KEY="isolated-test-only",
               HOLON_CONTROL_TOKEN=secrets.token_hex(32),
               HOLON_SOCKET_PATH=str(root / "daemon.sock"))
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    env["HOLON_HTTP_ADDR"] = f"127.0.0.1:{port}"
    base = f"http://127.0.0.1:{port}/api"
    held_run_started = root / "held-run-started"
    release_held_run = threading.Event()

    class FakeProvider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            if self.path != "/v1/chat/completions" or self.headers.get("Authorization") != "Bearer isolated-test-only":
                self.send_error(403)
                return
            if "Explicit stop contract: hold this request." in json.dumps(request.get("messages", [])):
                held_run_started.write_text("ready")
                release_held_run.wait(timeout=15)
            text = "Isolated iOS contract fixture reply."
            if request.get("stream"):
                chunks = [
                    {"id": "fixture-completion", "object": "chat.completion.chunk", "created": 1,
                     "model": request["model"], "choices": [{"index": 0,
                     "delta": {"role": "assistant", "content": text}, "finish_reason": None}]},
                    {"id": "fixture-completion", "object": "chat.completion.chunk", "created": 1,
                     "model": request["model"], "choices": [{"index": 0,
                     "delta": {}, "finish_reason": "stop"}]}]
                body = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks)
                        + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass  # The explicit stop cancels this provider request.
                return
            body = json.dumps({"id": "fixture-completion", "object": "chat.completion",
                "created": 1, "model": request.get("model", "fixture-model"),
                "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeProvider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    home = root / "holon"
    home.mkdir()
    (home / "config.json").write_text(json.dumps({
        "model": {"default": "ios-fixture/fixture-model"},
        "providers": {"ios-fixture": {"transport": "openai_chat_completions",
            "base_url": f"http://127.0.0.1:{provider.server_port}/v1",
            "auth": {"source": "env", "kind": "api_key", "env": "HOLON_IOS_TEST_KEY"}}}}))
    proxy = None
    with open(root / "daemon.log", "w+") as log:
        daemon = subprocess.Popen(
            [binary, "serve", "--access", "local", "--listen", f"127.0.0.1:{port}"],
            cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            for attempt in range(150):
                if daemon.poll() is not None:
                    raise RuntimeError(f"isolated daemon exited: {daemon.returncode}")
                try:
                    readiness = urllib.request.Request(base + "/handshake", headers={
                        "Authorization": "Bearer " + env["HOLON_CONTROL_TOKEN"]})
                    with urllib.request.urlopen(readiness, timeout=.3) as response:
                        if response.status == 200:
                            break
                except (OSError, urllib.error.URLError):
                    time.sleep(.1)
            else:
                raise RuntimeError("isolated daemon readiness timed out")

            class LocalControlConnection(http.client.HTTPConnection):
                def connect(self):
                    self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    self.sock.settimeout(self.timeout)
                    self.sock.connect(env["HOLON_SOCKET_PATH"])

            # Pairing issuance requires existing admission, even on loopback.
            # Use this isolated daemon's trusted local control transport.
            control = LocalControlConnection("localhost", timeout=3)
            try:
                control.request("POST", "/api/auth/pairing/issue")
                response = control.getresponse()
                if response.status != 200:
                    raise RuntimeError(f"local pairing issuance failed: {response.status}")
                pairing = json.loads(response.read())
                if not pairing.get("ticket") or not pairing.get("expires_at"):
                    raise RuntimeError("local pairing issuance returned no ticket or expiry")
            finally:
                control.close()

            class PrefixProxy(http.server.BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass

                dropped = False

                def forward(self):
                    prefix = "/isolated/holon/api/"
                    if not self.path.startswith(prefix):
                        self.send_error(404)
                        return
                    path = "/api/" + self.path[len(prefix):]
                    body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                    headers = {k: v for k, v in self.headers.items()
                               if k.lower() not in ("host", "connection", "content-length")}
                    upstream = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
                    try:
                        upstream.request(self.command, path, body=body, headers=headers)
                        response = upstream.getresponse()
                        data = response.read()
                        # Only discard a successful durable send acknowledgement.
                        with drop_lock:
                            drop = (self.command == "POST" and path.endswith("/prompt")
                                    and 200 <= response.status < 300 and not PrefixProxy.dropped)
                            if drop:
                                PrefixProxy.dropped = True
                        if drop:
                            self.close_connection = True
                            self.connection.shutdown(socket.SHUT_RDWR)
                            self.connection.close()
                            return
                        self.send_response(response.status)
                        self.send_header("Content-Type", response.getheader("Content-Type", "application/json"))
                        # response.read() preserves encoded bytes; forward their representation.
                        encoding = response.getheader("Content-Encoding")
                        if encoding:
                            self.send_header("Content-Encoding", encoding)
                        print(f"发送代理：HTTP {response.status}，Content-Encoding={encoding or 'identity'}",
                              flush=True)
                        self.send_header("Content-Length", str(len(data)))
                        self.end_headers()
                        self.wfile.write(data)
                    finally:
                        upstream.close()

                do_GET = forward
                do_POST = forward
                do_PUT = forward
                do_DELETE = forward

            drop_lock = threading.Lock()

            proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), PrefixProxy)
            threading.Thread(target=proxy.serve_forever, daemon=True).start()
            test_env = dict(os.environ)
            test_env.update(
                HOLON_SEND_BASE=base,
                HOLON_SEND_PAIRING_TICKET=pairing["ticket"],
                HOLON_SEND_HELD_RUN_STARTED=str(held_run_started),
                HOLON_SEND_PROXY_BASE=f"http://127.0.0.1:{proxy.server_port}/isolated/holon/api")
            print(f"真实隔离 daemon: {binary}; HTTP 与 prefix proxy 已就绪", flush=True)
            if os.environ.get("HOLON_SEND_FIXTURE_CHECK") == "1":
                print("ready fixture 配置与 pairing 已验证；未运行 Swift", flush=True)
                result = subprocess.CompletedProcess([], 0)
            else:
                test_sources = pathlib.Path(repo, "packages/client-sdk-swift/Tests")
                if not any("class LiveDaemonSendProbeTests" in path.read_text()
                           for path in test_sources.rglob("*.swift")):
                    raise RuntimeError("LiveDaemonSendProbeTests missing; refusing an empty Swift test run")
                result = subprocess.run(
                    ["swift", "test", "--package-path", repo + "/packages/client-sdk-swift",
                     "--filter", "LiveDaemonSendProbeTests"], env=test_env)
            if result.returncode:
                raise RuntimeError(f"Swift live probe failed: {result.returncode}")
        except BaseException:
            log.flush()
            log.seek(0)
            print(log.read(), file=sys.stderr)
            raise
        finally:
            release_held_run.set()
            if proxy:
                proxy.shutdown()
                proxy.server_close()
            provider.shutdown()
            provider.server_close()
            daemon.terminate()
            try:
                daemon.wait(timeout=8)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
PY
