#!/usr/bin/env python3
"""Dependency-free deterministic OpenAI Responses API stub."""
from __future__ import annotations
import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

def _matches(expected: Any, actual: Any) -> bool:
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(
            key in actual and _matches(value, actual[key]) for key, value in expected.items()
        )
    if isinstance(expected, list):
        return isinstance(actual, list) and len(expected) == len(actual) and all(
            _matches(left, right) for left, right in zip(expected, actual)
        )
    return expected == actual

class Transcript:
    def __init__(self, exchanges: list[dict[str, Any]]) -> None:
        self.exchanges, self.index, self.lock = exchanges, 0, threading.Lock()

    def consume(self, request: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        with self.lock:
            if self.index >= len(self.exchanges):
                return 409, {"error": {"type": "transcript_exhausted", "message": "no transcript response remains"}}
            exchange = self.exchanges[self.index]
            if not _matches(exchange.get("request", {}), request):
                return 409, {"error": {"type": "transcript_mismatch", "message": f"request did not match transcript step {self.index}"}}
            self.index += 1
            return int(exchange.get("status", 200)), exchange["response"]

def load_transcript(path: Path, scenario: str) -> Transcript:
    document = json.loads(path.read_text())
    scenarios = document.get("scenarios", document)
    exchanges = scenarios.get(scenario) if isinstance(scenarios, dict) else None
    if isinstance(exchanges, dict):
        exchanges = exchanges.get("transcript")
    if not isinstance(exchanges, list):
        raise ValueError(f"stub scenario is missing or invalid: {scenario}")
    for index, exchange in enumerate(exchanges):
        if not isinstance(exchange, dict) or not isinstance(exchange.get("request", {}), dict) or not isinstance(exchange.get("response"), dict):
            raise ValueError(f"invalid transcript exchange at index {index}")
    return Transcript(exchanges)

def make_handler(transcript: Transcript, request_log: Path) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        def _json(self, status: int, value: dict[str, Any]) -> None:
            body = json.dumps(value, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:
            self._json(200, {"status": "ok"}) if self.path == "/healthz" else self._json(404, {"error": {"type": "not_found", "message": "route not found"}})

        def do_POST(self) -> None:
            if self.path != "/v1/responses":
                self._json(404, {"error": {"type": "not_found", "message": "route not found"}})
                return
            try:
                request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                if not isinstance(request, dict):
                    raise ValueError
            except (ValueError, json.JSONDecodeError):
                self._json(400, {"error": {"type": "invalid_json", "message": "request body must be a JSON object"}})
                return
            request_log.parent.mkdir(parents=True, exist_ok=True)
            with request_log.open("a") as stream:
                stream.write(json.dumps(request, separators=(",", ":")) + "\n")
            self._json(*transcript.consume(request))

        def log_message(self, format: str, *args: Any) -> None:
            return
    return Handler

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument("--transcript", type=Path, required=True)
    parser.add_argument("--scenario", required=True)
    parser.add_argument("--request-log", type=Path, default=Path("/data/requests.jsonl"))
    args = parser.parse_args()
    ThreadingHTTPServer((args.listen, args.port), make_handler(load_transcript(args.transcript, args.scenario), args.request_log)).serve_forever()

if __name__ == "__main__":
    main()
