#!/usr/bin/env python3
"""Local-only network fault fixture; never calls a real model."""
from __future__ import annotations

import json
from email.utils import formatdate
import re
import select
import signal
import shutil
import socket
import socketserver
import sqlite3
import struct
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "openai_stub"))
from server import call_item, response, text_item  # noqa: E402
from task_recovery import RecoveryScenario  # noqa: E402
from continuation_cooldown import consume as consume_continuation_cooldown  # noqa: E402

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
        body = json.dumps(payload, ensure_ascii=False)
        side = self.path.split("/")[1]
        if setting("TASK_RESULT_RECOVERY", False):
            status, body = RECOVERY.consume_gated(payload, side, record)
            record(layer="mock_http", side=side, status=status, model=payload.get("model"),
                   stream=payload.get("stream", False))
            self.reply(status, body)
            return
        if consume_continuation_cooldown(self, payload, side, setting, record):
            return
        status = int(setting(f"STATUS_{side.upper()}", "200"))
        headers = retry_after_headers(side, status)
        scenario = setting("SCENARIO", "")
        markers = re.findall(r"QA_[A-Z0-9_]+", body)
        marker = next((value for value in (
            "QA_TIMER", "QA_TOOL_CONTINUE", "QA_COOL_A", "QA_COOL_CHAIN",
            "QA_PARK", "QA_FRESH"
        ) if value in markers), next(iter(markers), None))
        tool_output = "function_call_output" in body or "tool_result" in body
        if scenario == "tool-continuation" and side == "a" and "QA_COOL_A" in body:
            record(layer="mock_http", side=side, status=503, scenario=scenario,
                   marker=marker, tool_output=tool_output, response="cooling")
            self.reply(503, {"error": {"message": "fixture cooling", "type": "fixture"}},
                       {"Retry-After": "60"})
            return
        if scenario == "interjection" and "QA_COOL_CHAIN" in body and side == "a":
            record(layer="mock_http", side=side, status=503, scenario=scenario,
                   marker=marker, tool_output=tool_output, response="park")
            self.reply(503, {"error": {"message": "fixture parked", "type": "fixture"}},
                       {"Retry-After": "120"})
            return
        record(layer="mock_http", side=side, status=status, model=payload.get("model"),
               stream=payload.get("stream", False), scenario=scenario, marker=marker,
               tool_output=tool_output, retry_after=headers.get("Retry-After"))
        if status != 200:
            self.reply(status, {"error": {"message": "fixture failure", "type": "fixture"}},
                       headers)
            return
        if scenario == "tool-continuation":
            if tool_output:
                record(layer="mock_http", side=side, response="continuation")
                self.reply(200, response([text_item("QA_TOOL_CONTINUED")],
                                         f"resp_{side}_{len(RECORDS)}"))
            elif marker == "QA_TOOL_CONTINUE" and side == "a":
                record(layer="mock_http", side=side, marker=marker, response="tool_call")
                self.reply(200, response(
                    [call_item("call-tool", "ExecCommand", {
                        "cmd": "while [ ! -f /var/lib/holon/qa-3753-gate ]; "
                               "do sleep 0.2; done; printf QA_TOOL_DONE",
                        "yield_time_ms": 20000,
                    })], f"resp_{side}_{len(RECORDS)}"))
            else:
                self.reply(200, response([text_item("QA_COOL_HANDLED")],
                                         f"resp_{side}_{len(RECORDS)}"))
        elif scenario == "interjection":
            if setting("RECOVER_PARK", False):
                record(layer="mock_http", side=side, marker="QA_PARK",
                       response="recovered")
                self.reply(200, response([text_item("QA_PARK_DONE")],
                                         f"resp_{side}_{len(RECORDS)}"))
            elif "QA_TIMER" in body and not tool_output:
                record(layer="mock_http", side=side, marker="QA_TIMER",
                       response="tool_call",
                       response_held=bool(setting("HOLD_TIMER_RESPONSE", False)))
                deadline = time.monotonic() + 120
                while setting("HOLD_TIMER_RESPONSE", False):
                    if time.monotonic() >= deadline:
                        self.reply(504, {"error": {"message": "timer response gate timed out"}})
                        return
                    time.sleep(0.05)
                self.reply(200, response(
                    [call_item("call-timer", "ExecCommand", {
                        "cmd": "while [ ! -f /var/lib/holon/qa-3754-gate ]; "
                               "do sleep 0.2; done; printf QA_TIMER_TOOL_DONE",
                        "yield_time_ms": 20000,
                    })], f"resp_{side}_{len(RECORDS)}"))
            elif "QA_PARK" in body and not tool_output and side == "a":
                # Nonportable provider state keeps this continuation on A even
                # though a later independent timer can admit on healthy B.
                reasoning = {"type": "reasoning", "id": "rs_non_persisted",
                             "content": [{"type": "reasoning_text",
                                          "text": "QA_PROVIDER_OWNED_REASONING"}],
                             "encrypted_content": "opaque-reasoning"}
                record(layer="mock_http", side=side, marker="QA_PARK",
                       response="tool_call", tool_output=tool_output,
                       provider_owned_reasoning=reasoning)
                self.reply(200, response(
                    [reasoning, call_item("call-park-tool", "ExecCommand", {
                        "cmd": "while [ ! -f /var/lib/holon/qa-3754-park-gate ]; "
                               "do sleep 0.2; done; printf QA_PARK_TOOL_DONE",
                        "yield_time_ms": 20000,
                    })], f"resp_{side}_{len(RECORDS)}"))
            elif tool_output:
                self.reply(200, response([text_item("QA_TIMER_DONE")],
                                         f"resp_{side}_{len(RECORDS)}"))
            else:
                self.reply(200, response([text_item("QA_OTHER_DONE")],
                                         f"resp_{side}_{len(RECORDS)}"))
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
    source = candidates[0]
    # A live WAL database cannot be opened read-only through the read-only
    # evidence mount; copy the database plus sidecars before reading.
    scratch = tempfile.TemporaryDirectory()
    target = Path(scratch.name) / "runtime.sqlite"
    shutil.copy2(source, target)
    for suffix in ("-wal", "-shm"):
        sidecar = Path(str(source) + suffix)
        if sidecar.exists():
            shutil.copy2(sidecar, Path(str(target) + suffix))
    db = sqlite3.connect(f"file:{target}", uri=True)
    db.row_factory = sqlite3.Row
    db.execute("BEGIN")
    tables = [r[0] for r in db.execute("select name from sqlite_master where type='table'")]
    counts = {t: db.execute(f'SELECT count(*) FROM "{t}"').fetchone()[0]
              for t in tables if not t.startswith("sqlite_")}
    rows, truncated = {}, []
    for table in tables:
        if (any(word in table for word in ("turn", "work_item", "queue", "recovery",
                                          "settlement", "provider_admission",
                                          "incident", "tool", "message", "activation_input",
                                          "transcript", "input_assignment"))
                or route_throttle and table.endswith("_incidents")
                or (task_result or route_throttle) and table in {
                    "tasks", "messages", "briefs", "wait_conditions",
                    "execution_protocol_attempts", "execution_protocol_outcomes",
                }):
            cursor = db.execute(f'SELECT * FROM "{table}"')
            rows[table] = []
            # Task-result drills can outgrow 500 messages during real backoff.
            # Keep one read transaction, reading every row in bounded batches.
            while batch := cursor.fetchmany(500):
                rows[table].extend(dict(r) for r in batch)
                if not task_result:
                    break
            if counts[table] > len(rows[table]):
                truncated.append(table)
    db.close()
    scratch.cleanup()
    print(json.dumps({"counts": counts, "rows": rows, "truncated_tables": truncated},
                     default=str))


def main():
    mode = sys.argv[1]
    if mode == "mock":
        ThreadingHTTPServer(("0.0.0.0", 8080), Mock).serve_forever()
    elif mode == "proxy":
        for side, port in (("a", 8081), ("b", 8082), ("c", 8083)):
            server = ProxyServer(("0.0.0.0", port), Proxy)
            server.side = side
            threading.Thread(target=server.serve_forever, daemon=True).start()
        signal.pause()
    elif mode == "controller":
        signal.pause()
    elif mode == "request":
        request()
    elif mode == "snapshot":
        database()
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
