#!/usr/bin/env python3
"""Local-only network fault fixture; never calls a real model."""
from __future__ import annotations

import json
from email.utils import formatdate
import select
import signal
import socket
import socketserver
import sqlite3
import struct
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "openai_stub"))
from server import response, text_item  # noqa: E402
from task_recovery import RecoveryScenario  # noqa: E402

LOCK = threading.Lock()
RECORDS: list[dict] = []
RECOVERY = RecoveryScenario()


def setting(name, default):
    path = Path("/tmp/qa-fault.json")
    return json.loads(path.read_text()).get(name, default) if path.exists() else default


def record(**fields):
    row = {"at": time.time(), "monotonic": time.monotonic(), **fields}
    with LOCK:
        RECORDS.append(row)
    print(json.dumps(row), flush=True)


def retry_after_headers(side, status):
    if status != 429:
        return {}
    value = setting(f"RETRY_AFTER_{side.upper()}", "1")
    if isinstance(value, dict):
        seconds = int(value["seconds"])
        value = (formatdate(time.time() + seconds, usegmt=True)
                 if value["format"] == "http-date" else str(seconds))
    return {} if value is None else {"Retry-After": str(value)}


class Mock(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_GET(self):
        self.reply(200, {"records": RECORDS})

    def reply(self, status, value, headers=None):
        raw = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Connection", "close")
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(raw)
        self.close_connection = True

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        side = self.path.split("/")[1]
        if setting("TASK_RESULT_RECOVERY", False):
            status, body = RECOVERY.consume_gated(payload, side, record)
            record(layer="mock_http", side=side, status=status, model=payload.get("model"),
                   stream=payload.get("stream", False))
            self.reply(status, body)
            return
        status = int(setting(f"STATUS_{side.upper()}", "200"))
        headers = retry_after_headers(side, status)
        record(layer="mock_http", side=side, status=status, model=payload.get("model"),
               stream=payload.get("stream", False), retry_after=headers.get("Retry-After"))
        if status != 200:
            self.reply(status, {"error": {"message": "fixture failure", "type": "fixture"}},
                       headers)
        else:
            # The fixture state does not advance on a failed request.
            self.reply(200, response([text_item("QA_FALLBACK_COMPLETED")],
                                     f"resp_{side}_{len(RECORDS)}"))


class Proxy(socketserver.BaseRequestHandler):
    def handle(self):
        side = self.server.side
        mode = setting(f"FAULT_{side.upper()}", "pass")
        record(layer="proxy_tcp", side=side, fault=mode)
        if mode == "rst":
            self.request.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER,
                                    struct.pack("ii", 1, 0))
            return
        if mode.startswith("blackhole"):
            self.request.settimeout(10)
            try:
                self.request.recv(65536)
                if mode == "blackhole_body":
                    self.request.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                                         b"Content-Length: 100000\r\n\r\n{")
                elif mode == "blackhole_idle":
                    self.request.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"
                                         b"Connection: close\r\n\r\n"
                                         b"event: response.created\ndata: {\"type\":\"response.created\","
                                         b"\"response\":{\"id\":\"resp_idle\",\"status\":\"in_progress\"}}\n\n")
                # An upper bound for the fixture, not the product's deadline.
                time.sleep(120)
            except (OSError, TimeoutError):
                pass
            return
        upstream_timeout = float(setting("UPSTREAM_TIMEOUT", 10))
        with socket.create_connection(("mock", 8080), timeout=upstream_timeout) as upstream:
            peers = [self.request, upstream]
            while True:
                readable, _, _ = select.select(peers, [], [], max(30, upstream_timeout))
                if not readable:
                    return
                for source in readable:
                    try:
                        data = source.recv(65536)
                        if not data:
                            return
                        (upstream if source is self.request else self.request).sendall(data)
                    except OSError:
                        return


class ProxyServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def request():
    command = json.load(sys.stdin)
    body = command.get("body")
    headers = {"Content-Type": "application/json"}
    if command.get("token"):
        headers["Authorization"] = "Bearer " + command["token"]
    req = urllib.request.Request(command["url"], method=command.get("method", "GET"),
                                 headers=headers,
                                 data=None if body is None else json.dumps(body).encode())
    try:
        with urllib.request.urlopen(req, timeout=10) as result:
            print(json.dumps({"status": result.status, "body": json.load(result)}))
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            raw = json.loads(raw)
        except ValueError:
            pass
        print(json.dumps({"status": error.code, "body": raw}))


def database(*, task_result=False, route_throttle=False):
    candidates = list(Path("/evidence-home").glob("**/runtime.sqlite"))
    if len(candidates) != 1:
        raise RuntimeError(f"expected one isolated runtime database, found {len(candidates)}")
    db = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    db.execute("BEGIN")
    tables = [r[0] for r in db.execute("select name from sqlite_master where type='table'")]
    counts = {t: db.execute(f'SELECT count(*) FROM "{t}"').fetchone()[0]
              for t in tables if not t.startswith("sqlite_")}
    rows, truncated = {}, []
    for table in tables:
        if (any(word in table for word in ("turn", "work_item", "queue", "recovery",
                                          "settlement", "provider_admission"))
                or route_throttle and table.endswith("_incidents")
                or (task_result or route_throttle) and table in {
                    "tasks", "messages", "briefs", "wait_conditions",
                    "execution_protocol_attempts", "execution_protocol_outcomes",
                }):
            rows[table] = [dict(r) for r in db.execute(f'SELECT * FROM "{table}" LIMIT 500')]
            if counts[table] > len(rows[table]):
                truncated.append(table)
    print(json.dumps({"counts": counts, "rows": rows, "truncated_tables": truncated},
                     default=str))


def main():
    mode = sys.argv[1]
    if mode == "mock":
        ThreadingHTTPServer(("0.0.0.0", 8080), Mock).serve_forever()
    elif mode == "proxy":
        for side, port in (("a", 8081), ("b", 8082)):
            server = ProxyServer(("0.0.0.0", port), Proxy)
            server.side = side
            threading.Thread(target=server.serve_forever, daemon=True).start()
        signal.pause()
    elif mode == "controller":
        signal.pause()
    elif mode == "request":
        request()
    elif mode == "database":
        database()
    elif mode == "task-result-database":
        database(task_result=True)
    elif mode == "route-throttle-database":
        database(route_throttle=True)
    else:
        raise SystemExit("unknown fixture mode")


if __name__ == "__main__":
    main()
