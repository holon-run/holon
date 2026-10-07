#!/usr/bin/env python3
"""Run a real, isolated Holon daemon with explicitly scripted demo conversations.

No cloud model is contacted. This is a capture environment, not a model benchmark.
Keep --home outside the repository and never point it at an existing Holon home.
"""

import argparse
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen


SCENARIOS = [
    (
        "demo-release", "Release Manager · Demo",
        "Demo task: draft an Android internal-test release checklist. "
        "Do not upload a build or publish a production release.",
        "## Ready for internal testing\n\n"
        "A demonstration checklist, not a real release result.\n\n"
        "1. Verify the signing certificate and an unused version code.\n"
        "2. Confirm store assets, privacy policy and reviewer access.\n"
        "3. Ask the maintainer to approve the manual internal workflow.\n\n"
        "**Boundary:** no PR merge, no production release, no duplicate upload.",
    ),
    (
        "demo-review", "Code Reviewer · Demo",
        "Demo task: review an attachment-upload design for permissions, "
        "failure recovery and privacy boundaries. Do not access a real repository.",
        "## Attachment upload review\n\n"
        "A demonstration design review; no real code was inspected.\n\n"
        "- **Permissions:** use only files explicitly selected by the user.\n"
        "- **Recovery:** keep drafts offline; retries must not send duplicates.\n"
        "- **Privacy:** show the destination host and never log credentials.\n\n"
        "Next: exercise cancel, retry and sign-out using isolated sample data.",
    ),
    (
        "demo-docs", "Docs Steward · Demo",
        "Demo task: draft a quick start for connecting to a self-hosted Holon. "
        "Remind users not to put secrets in chat.",
        "## Your agents, wherever you are\n\n"
        "A demonstration quick-start draft.\n\n"
        "1. Start Holon on your own host and check its address.\n"
        "2. Add the network in Android and sign in with authorization.\n"
        "3. Open an agent, send a task and read its result brief.\n\n"
        "**Stay safe:** prefer HTTPS. Never send tokens, passwords or real "
        "personal information in chat.",
    ),
    (
        "demo-android", "Android Developer · Demo",
        "Demo task: plan session-recovery checks for offline use, network "
        "switching and draft restoration. Do not touch real app data.",
        "## Session recovery plan\n\n"
        "A demonstration plan, not an executed test report.\n\n"
        "- Offline: preserve unsent drafts and show a recoverable state.\n"
        "- Switch hosts: keep sessions and caches isolated per network.\n"
        "- Restart: restore the selected agent without resending a prompt.\n\n"
        "Use a dedicated emulator and isolated host, never production data.",
    ),
]

DEMO_WORK_ITEM = {
    "objective": "Demo: prepare an Android internal-test checklist",
    "plan_status": "needs_input",
    "plan": (
        "# Internal-test checklist · Demo\n\n"
        "This is a sample work item, not a real release approval.\n\n"
        "## Scope\n"
        "Review sample store materials and record the remaining release gates.\n"
        "Do not upload a build, publish a release or access production data.\n\n"
        "## Acceptance\n"
        "The maintainer must confirm privacy, reviewer access and versioning "
        "before any actual release operation.\n"
    ),
    "todo_list": [
        {"state": "completed", "text": "Review the sample store artwork"},
        {"state": "in_progress", "text": "Prepare the internal-test checklist"},
        {"state": "pending", "text": "Confirm privacy policy and reviewer access"},
        {"state": "pending", "text": "Ask the maintainer before any upload"},
    ],
}


def seed_demo_files(home):
    """Write disclosed sample artifacts only inside this new capture home."""
    folder = home / "agents" / "demo-release" / "work" / "store-demo"
    folder.mkdir(parents=True)
    (folder / "release-checklist.md").write_text(
        DEMO_WORK_ITEM["plan"], encoding="utf-8"
    )
    (folder / "privacy-notes.md").write_text(
        "# Privacy notes · Demo\n\n"
        "Sample documentation, not a deployed policy or compliance result.\n\n"
        "- Connect only to a host you are authorized to use.\n"
        "- Host operators choose models, integrations and retention rules.\n"
        "- Never include tokens or personal data in store screenshots.\n",
        encoding="utf-8",
    )
    (folder / "reviewer-access.md").write_text(
        "# Reviewer access · Demo\n\n"
        "Use an isolated test host with fictional conversations.\n"
        "This sample contains no account, password or access token.\n",
        encoding="utf-8",
    )
    (folder / "asset-manifest.json").write_text(
        json.dumps({"demo": True, "language": "en-US",
                    "screens": ["agents", "conversation", "work", "files"]},
                   indent=2) + "\n",
        encoding="utf-8",
    )


class DemoProvider(BaseHTTPRequestHandler):
    """A loopback-only Responses transport serving disclosed, scripted text."""

    def log_message(self, *_args):
        pass

    def do_GET(self):
        body = json.dumps({"object": "list", "data": [
            {"id": "gpt-5.4", "object": "model", "owned_by": "scripted-demo"}
        ]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        print(f"Scripted provider request: {self.path}", flush=True)
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        text = json.dumps(request.get("input", []), ensure_ascii=False)
        answer = "Welcome to the demo workspace. Explore the four sample agents."
        # Match the explicit user prompt, not system instructions or agent identity.
        for _agent_id, _name, prompt, result in SCENARIOS:
            if prompt in text:
                answer = result
        work_item_result = any(
            item.get("type") == "function_call_output"
            and item.get("call_id") == "call_demo_work_item"
            for item in request.get("input", []) if isinstance(item, dict)
        )
        if work_item_result or DEMO_WORK_ITEM["objective"] in text:
            answer = SCENARIOS[0][3]
        response = {
            "id": "resp_demo_" + secrets.token_hex(8),
            "status": "completed",
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "output": [{
                "type": "message", "id": "msg_demo_" + secrets.token_hex(8),
                "status": "completed", "role": "assistant",
                "content": [{"type": "output_text", "text": answer}],
            }],
        }
        if SCENARIOS[0][2] in text and not work_item_result:
            tool_name = next(
                tool["name"] for tool in request.get("tools", [])
                if tool.get("name") == "CreateWorkItem"
            )
            # Execute the normal runtime tool: do not fabricate database state.
            response["output"] = [{
                "type": "function_call",
                "id": "fc_demo_work_item",
                "call_id": "call_demo_work_item",
                "name": tool_name,
                "arguments": json.dumps(DEMO_WORK_ITEM),
                "status": "completed",
            }]
        event = {"type": "response.completed", "response": response}
        streaming = request.get("stream", False)
        body = (
            "event: response.completed\ndata: "
            + json.dumps(event, ensure_ascii=False) + "\n\n"
            if streaming else json.dumps(response, ensure_ascii=False)
        ).encode()
        self.send_response(200)
        self.send_header(
            "Content-Type", "text/event-stream" if streaming else "application/json"
        )
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--holon", default="holon")
    parser.add_argument("--port", type=int, default=17980)
    parser.add_argument("--provider-port", type=int, default=17981)
    args = parser.parse_args()
    if args.home.exists():
        parser.error("--home must not exist; use a fresh dedicated directory")
    args.home.mkdir(parents=True, mode=0o700)
    token = secrets.token_urlsafe(32)
    token_file = args.home / "capture-token"
    token_file.write_text(token)
    token_file.chmod(0o600)
    provider = ThreadingHTTPServer(("127.0.0.1", args.provider_port), DemoProvider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    # Preserve inherited runtime caller context; override only this demo's routing.
    env = dict(os.environ)
    env.update({
        "HOLON_HOME": str(args.home.resolve()),
        "HOLON_AGENT_ID": "demo-studio",
        "HOLON_MODEL": "openai/gpt-5.4",
        "HOLON_DISABLE_PROVIDER_FALLBACK": "true",
        "OPENAI_API_KEY": "scripted-demo-not-a-cloud-key",
        "HOLON_OPENAI_BASE_URL": f"http://127.0.0.1:{args.provider_port}/v1",
    })
    with (args.home / "daemon.log").open("w") as log:
        daemon = subprocess.Popen([
            args.holon, "serve", "--listen", f"127.0.0.1:{args.port}",
            "--token-file", str(token_file),
        ], env=env, stdout=log, stderr=subprocess.STDOUT)
        stopped = threading.Event()
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, lambda *_: stopped.set())

        def api(path, payload=None):
            request = Request(
                f"http://127.0.0.1:{args.port}/api{path}",
                data=json.dumps(payload).encode() if payload is not None else None,
                headers={"Authorization": f"Bearer {token}",
                         "Content-Type": "application/json"},
            )
            with urlopen(request, timeout=10) as response:
                return json.load(response)

        try:
            deadline = time.monotonic() + 30
            while True:
                if daemon.poll() is not None:
                    raise RuntimeError("demo daemon exited; inspect its private log")
                try:
                    api("/agents/list")
                    break
                except OSError:
                    if time.monotonic() > deadline:
                        raise RuntimeError("demo daemon did not become ready")
                    stopped.wait(0.25)
            for agent_id, name, prompt, _result in SCENARIOS:
                api(f"/control/agents/{agent_id}/create", {"name": name})
                api(f"/control/agents/{agent_id}/prompt", {"text": prompt})
            seed_demo_files(args.home)
            print(f"Demo ready: http://127.0.0.1:{args.port}/api", flush=True)
            print("4 scripted demo agents seeded; no real model calls or user data.",
                  flush=True)
            while not stopped.wait(1):
                if daemon.poll() is not None:
                    raise RuntimeError("demo daemon exited")
        finally:
            if daemon.poll() is None:
                daemon.terminate()
                try:
                    daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()
            provider.shutdown()
            token_file.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
