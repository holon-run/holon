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
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

binary, repo = sys.argv[1:]
with tempfile.TemporaryDirectory(prefix="holon-ios-contract-") as temporary:
    root = pathlib.Path(temporary)
    # Do not inherit provider credentials, production paths or daemon settings.
    env = {k: v for k, v in os.environ.items()
           if k in ("PATH", "TMPDIR", "LANG", "SystemRoot")
           or k.startswith("HOLON_CALLER_")}
    env.update(HOME=str(root), HOLON_HOME=str(root / "holon"),
               HOLON_WORKSPACE_DIR=str(root), HOLON_BOOTSTRAP="1",
               HOLON_SOCKET_PATH=str(root / "daemon.sock"))
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    env["HOLON_HTTP_ADDR"] = f"127.0.0.1:{port}"
    base = f"http://127.0.0.1:{port}/api"
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
                    with urllib.request.urlopen(base + "/handshake", timeout=.3) as response:
                        if response.status == 200:
                            break
                except (OSError, urllib.error.URLError):
                    time.sleep(.1)
            else:
                raise RuntimeError("isolated daemon readiness timed out")

            class PrefixProxy(http.server.BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass

                def do_GET(self):
                    prefix = "/isolated/holon/api/"
                    if not self.path.startswith(prefix):
                        self.send_error(404)
                        return
                    upstream = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
                    try:
                        upstream.request("GET", "/api/" + self.path[len(prefix):])
                        response = upstream.getresponse()
                        self.send_response(response.status)
                        self.send_header("Content-Type", response.getheader("Content-Type", ""))
                        if self.path.endswith("/events/stream"):
                            # Real upstream connected; deliberately terminate
                            # downstream cleanly to exercise fresh bootstrap.
                            self.send_header("Content-Length", "0")
                            self.end_headers()
                        else:
                            body = response.read()
                            self.send_header("Content-Length", str(len(body)))
                            self.end_headers()
                            self.wfile.write(body)
                    finally:
                        upstream.close()

            proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), PrefixProxy)
            threading.Thread(target=proxy.serve_forever, daemon=True).start()
            test_env = dict(os.environ)
            test_env.update(
                HOLON_LIVE_BASE=base,
                HOLON_LIVE_PROXY_BASE=f"http://127.0.0.1:{proxy.server_port}/isolated/holon/api")
            print(f"真实隔离 daemon: {binary}; HTTP 与 prefix proxy 已就绪", flush=True)
            result = subprocess.run(
                ["swift", "test", "--package-path", repo + "/packages/client-sdk-swift",
                 "--filter", "LiveDaemonProbeTests"], env=test_env)
            if result.returncode:
                raise RuntimeError(f"Swift live probe failed: {result.returncode}")
        except BaseException:
            log.flush()
            log.seek(0)
            print(log.read(), file=sys.stderr)
            raise
        finally:
            if proxy:
                proxy.shutdown()
                proxy.server_close()
            daemon.terminate()
            try:
                daemon.wait(timeout=8)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
PY
