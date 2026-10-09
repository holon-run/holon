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
import struct
import zlib

from ios_simulator_text_size import (
    MAXIMUM_TEXT_SIZE, initialize_simulator_text_size, runtime_text_size_control,
    simulator_text_size, simulator_appearance,
)

binary, repo, mode = sys.argv[1:]
rich_acceptance = os.environ.get("IOS_RICH_ACCEPTANCE") == "1"
rich_activity_acceptance = rich_acceptance and (
    mode == "--sdk-only" or not os.environ.get("IOS_UI_CASES")
    or "testRichActivityWorkflow" in os.environ["IOS_UI_CASES"].split(","))
lost_response_acceptance = os.environ.get("IOS_LOST_RESPONSE_ACCEPTANCE") == "1"
history_acceptance = os.environ.get("IOS_HISTORY_ACCEPTANCE") == "1"
share_acceptance = os.environ.get("IOS_SHARE_ACCEPTANCE") == "1"
with tempfile.TemporaryDirectory(prefix="holon-ios-ui-") as temporary:
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
        image_requests = 0
        rich_batches = 0

        def log_message(self, *args):
            pass

        def do_GET(self):
            FakeProvider.image_requests += 1
            self.send_error(403)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            if self.path != "/v1/chat/completions" or self.headers.get("Authorization") != "Bearer isolated-test-only":
                self.send_error(403)
                return
            if "Explicit stop contract: hold this request." in json.dumps(request.get("messages", [])):
                held_run_started.write_text("ready")
                release_held_run.wait(timeout=15)
            tool_calls = None
            if rich_activity_acceptance and "IOS_RICH_ACTIVITIES" in json.dumps(request.get("messages", [])) and FakeProvider.rich_batches < 10:
                available = {tool.get("function", {}).get("name") for tool in request.get("tools", [])}
                if "GetAgent" not in available:
                    self.send_error(500, "GetAgent must be a real supported read-only tool")
                    return
                batch = FakeProvider.rich_batches
                FakeProvider.rich_batches += 1
                tool_calls = [{"id": f"fixture-read-{batch}-{index}", "type": "function",
                               "function": {"name": "GetAgent", "arguments": "{}"}}
                              for index in range(6)]
            text = ("## Result\n\nIOS_POPULATED_BRIEF: isolated iOS contract fixture reply.\n\n"
                    "**Ready** · 中英文混排\n\n- [x] Read result\n- [ ] Inspect files\n\n"
                    "> Keep operator output separate from execution.\n\n"
                    "| File | Status |\n| --- | --- |\n| report.md | Ready |\n\n"
                    "```swift\nlet message = \"Hello Holon\"\n```\n\n"
                    f"![Explicit image only](http://127.0.0.1:{provider.server_port}/must-not-auto-load.png)\n\n"
                    f"[Open fixture file]({root / 'holon' / 'agents' / 'holon-tester' / 'ios-populated.txt'})\n\n"
                    f"File: {root / 'holon' / 'agents' / 'holon-tester' / 'ios-populated.txt'}")
            if tool_calls:
                text = f"IOS_RICH_ASSISTANT: read-only inspection batch {FakeProvider.rich_batches}."
            elif history_acceptance:
                text = "IOS_POPULATED_BRIEF: History fixture result."
            finish_reason = "tool_calls" if tool_calls else "stop"
            if request.get("stream"):
                chunks = [
                    {"id": "fixture-completion", "object": "chat.completion.chunk", "created": 1,
                     "model": request["model"], "choices": [{"index": 0,
                     "delta": {"role": "assistant", "content": text,
                               **({"tool_calls": [dict(call, index=index) for index, call in enumerate(tool_calls)]} if tool_calls else {})}, "finish_reason": None}]},
                    {"id": "fixture-completion", "object": "chat.completion.chunk", "created": 1,
                     "model": request["model"], "choices": [{"index": 0,
                     "delta": {}, "finish_reason": finish_reason}]}]
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
                "choices": [{"index": 0, "message": {"role": "assistant", "content": text,
                             **({"tool_calls": tool_calls} if tool_calls else {})},
                             "finish_reason": finish_reason}],
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
    proxy = None
    release_lost_response = threading.Event()
    lost_receipts = []
    lost_lock = threading.Lock()
    proxy_control_token = secrets.token_hex(32)

    class LostResponseProxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass  # Never log session headers or pairing tickets.

        def do_GET(self):
            self.forward()

        def do_POST(self):
            if self.path == "/fixture/release-lost-response":
                if self.headers.get("Authorization") != "Bearer " + proxy_control_token:
                    self.send_error(403); return
                release_lost_response.set()
                self.send_response(204); self.end_headers(); return
            self.forward()

        def forward(self):
            length = int(self.headers.get("Content-Length", "0"))
            if length > 32 * 1024 * 1024:
                self.send_error(413); return
            body = self.rfile.read(length) if length else None
            # Native foreground SSE can be quiet between heartbeats. The proxy
            # must not manufacture unrelated offline failures during acceptance.
            upstream = http.client.HTTPConnection("127.0.0.1", port,
                                                  timeout=90 if self.command == "GET" else 15)
            try:
                headers = {key: value for key, value in self.headers.items()
                           if key.lower() not in ("host", "connection", "transfer-encoding")}
                # Parse the actual receipt rather than URLSession's compressed
                # wire representation. Other responses remain transparent.
                headers["Accept-Encoding"] = "identity"
                upstream.request(self.command, self.path, body=body, headers=headers)
                response = upstream.getresponse()
                is_lost_prompt = (self.command == "POST" and self.path.endswith("/prompt")
                    and body and ("IOS_LOST_RESPONSE_SEND" in json.loads(body).get("text", "")
                        or (share_acceptance and "IOS_SHARED_TEXT" in json.loads(body).get("text", ""))))
                if is_lost_prompt:
                    data = response.read(1024 * 1024 + 1)
                    if response.status != 200 or len(data) > 1024 * 1024:
                        raise RuntimeError("lost-response fixture prompt was not accepted")
                    receipt = json.loads(data)
                    with lost_lock:
                        lost_receipts.append((json.loads(body)["client_request_id"], receipt))
                        attempt = len(lost_receipts)
                    delivered = release_lost_response.is_set()
                    print(f"Lost-response attempt {attempt}: {receipt.get('disposition')}, delivered={delivered}", flush=True)
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(data)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    if delivered:
                        self.wfile.write(data); self.wfile.flush()
                    # Until the runner explicitly releases it, accept on the real
                    # daemon but deliberately close with a truncated response body.
                    self.close_connection = True
                    return
                self.send_response(response.status)
                for key, value in response.getheaders():
                    if key.lower() not in ("connection", "transfer-encoding"):
                        self.send_header(key, value)
                self.end_headers()
                while True:
                    data = response.read1(65_536)
                    if not data:
                        break
                    self.wfile.write(data); self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, socket.timeout, http.client.IncompleteRead):
                pass  # Native backgrounding closes its foreground SSE connection.
            finally:
                upstream.close()

    if lost_response_acceptance:
        proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), LostResponseProxy)
        proxy.daemon_threads = True
        threading.Thread(target=proxy.serve_forever, daemon=True).start()
    home = root / "holon"
    home.mkdir()
    (home / "config.json").write_text(json.dumps({
        "model": {"default": "ios-fixture/fixture-model"},
        "providers": {"ios-fixture": {"transport": "openai_chat_completions",
            "base_url": f"http://127.0.0.1:{provider.server_port}/v1",
            "auth": {"source": "env", "kind": "api_key", "env": "HOLON_IOS_TEST_KEY"}}}}))
    task_id = None
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

            def local(method, path, payload=None):
                connection = LocalControlConnection("localhost", timeout=10)
                try:
                    connection.request(method, "/api" + path,
                        body=json.dumps(payload).encode() if payload is not None else None,
                        headers={"Content-Type": "application/json"})
                    response = connection.getresponse()
                    data = json.loads(response.read())
                    if response.status != 200:
                        raise RuntimeError(f"fixture control {path}: HTTP {response.status}: {data}")
                    return data
                finally:
                    connection.close()

            agent = "holon-tester"
            local("POST", f"/control/agents/{agent}/create", {})
            work = local("POST", f"/control/agents/{agent}/work-items", {"objective": "IOS_POPULATED_WORK"})
            work_id = work["id"]
            plan = pathlib.Path(work["plan_artifact"]["path"])
            if not plan.resolve().is_relative_to(root.resolve()):
                raise RuntimeError("plan escapes fixture root")
            plan.write_text("# IOS_POPULATED_PLAN\n\n" + "Real isolated work plan.\n" * 500
                            + "\nIOS_POPULATED_FULL_PLAN\n")
            local("POST", f"/agents/{agent}/enqueue", {"text": "IOS_POPULATED_BRIEF: Produce the isolated fixture reading brief."})
            for attempt in range(150):
                conversation = local("GET", f"/agents/{agent}/conversation")
                if "IOS_POPULATED_BRIEF" in json.dumps(conversation):
                    break
                time.sleep(.1)
            else:
                raise RuntimeError("real conversation brief timed out")
            # The deterministic provider returns a real brief; wait for the run to settle.
            for attempt in range(150):
                try:
                    completed = local("POST", f"/control/agents/{agent}/work-items/{work_id}/complete",
                                      {"report_text": "IOS_POPULATED_BRIEF: isolated work completed."})
                    break
                except RuntimeError as error:
                    if "active" not in str(error) and "409" not in str(error):
                        raise
                    time.sleep(.1)
            else:
                raise RuntimeError("work completion timed out")
            file_path = "ios-populated.txt"
            workspace = root / "holon" / "agents" / agent
            (workspace / file_path).write_text("IOS_POPULATED_FILE\n")
            state = local("GET", f"/agents/{agent}/state")
            workspace_id = next(w["workspace_id"] for w in state["workspace"]["workspaces"]
                                if w.get("kind") == "agent_home" or w.get("workspace_id", "").startswith("agent_home"))
            rich_turn = None
            rich_additional_work = None
            if history_acceptance:
                for number in range(25):
                    marker = f"IOS_HISTORY_{number:03}"
                    local("POST", f"/agents/{agent}/enqueue", {"text": marker})
                    for attempt in range(200):
                        history = local("GET", f"/agents/{agent}/conversation")
                        if any(marker in json.dumps(turn) and turn.get("brief_ids")
                               for turn in history.get("turns", [])):
                            break
                        time.sleep(.1)
                    else:
                        raise RuntimeError("history fixture turn did not finish")
            if rich_activity_acceptance:
                # All rich data lives in this temporary daemon, never a production Agent.
                for number in range(90):
                    local("POST", f"/control/agents/ios-fixture-agent-{number:03}/create", {})
                for number in range(55):
                    item = local("POST", f"/control/agents/{agent}/work-items", {"objective": f"IOS_RICH_WORK_{number:03}"})
                    # Keep the catalog large without fifty-five unfinished items
                    # repeatedly scheduling turns during the native reader test.
                    for attempt in range(150):
                        try:
                            local("POST", f"/control/agents/{agent}/work-items/{item['id']}/complete",
                                  {"report_text": "Isolated catalog fixture completed."})
                            break
                        except RuntimeError as error:
                            if "active" not in str(error) and "409" not in str(error):
                                raise
                            time.sleep(.1)
                    else:
                        raise RuntimeError("rich catalog work did not settle")
                initial_work = local("GET", f"/agents/{agent}/work-items?limit=50")
                expanded_work = local("GET", f"/agents/{agent}/work-items?limit=100")
                def work_ids(reply):
                    return [item["id"] for item in (reply if isinstance(reply, list) else reply["items"])]
                first_ids = set(work_ids(initial_work))
                additional = [item for item in work_ids(expanded_work) if item not in first_ids]
                if len(first_ids) != 50 or len(work_ids(expanded_work)) != 56 or not additional:
                    raise RuntimeError("rich work acceptance requires a reachable fifty-to-fifty-six window")
                rich_additional_work = additional[0]
            if rich_acceptance:
                rich_files = workspace / "rich-files"
                rich_files.mkdir()
                for number in range(80):
                    (rich_files / f"note-{number:03}.txt").write_text(f"IOS_RICH_NOTE_{number:03}\n")
                (rich_files / "large-utf8.txt").write_text(
                    "IOS_RICH_TEXT_START\n" + "中英文 UTF-8 complete source line.\n" * 18000 + "IOS_RICH_TEXT_END\n")
                (rich_files / "source.ts").write_text(
                    '// IOS_RICH_CODE_START\n' + 'const value: string = "Holon 中英文";\n' * 16000 + '// IOS_RICH_CODE_END\n')
                (rich_files / "report.md").write_text("# Native report\n\n[Open sibling](./note-079.txt)\n\n**Full result**.\n")
                # Passive synthetic image/PDF; no remote resource or executable action.
                def chunk(kind, data):
                    return struct.pack("!I", len(data)) + kind + data + struct.pack("!I", zlib.crc32(kind + data))
                pixels = b"\0" + bytes([40, 100, 190]) * 64
                (rich_files / "image.png").write_bytes(b"\x89PNG\r\n\x1a\n" +
                    chunk(b"IHDR", struct.pack("!2I5B", 64, 64, 8, 2, 0, 0, 0)) +
                    chunk(b"IDAT", zlib.compress(pixels * 64)) + chunk(b"IEND", b""))
                objects = [b"<< /Type /Catalog /Pages 2 0 R >>",
                    b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>",
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>",
                    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
                for page in (1, 2):
                    stream = f"BT /F1 18 Tf 20 150 Td (IOS RICH PDF PAGE {page}) Tj ET".encode()
                    objects.append(f"<< /Length {len(stream)} >>\nstream\n".encode() + stream + b"\nendstream")
                pdf, offsets = b"%PDF-1.4\n", [0]
                for number, value in enumerate(objects, 1):
                    offsets.append(len(pdf)); pdf += f"{number} 0 obj\n".encode() + value + b"\nendobj\n"
                xref = len(pdf)
                pdf += f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode()
                pdf += b"".join(f"{offset:010} 00000 n \n".encode() for offset in offsets[1:])
                pdf += f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
                (rich_files / "report.pdf").write_bytes(pdf)
            if rich_activity_acceptance:
                local("POST", f"/agents/{agent}/enqueue", {"text": "IOS_RICH_ACTIVITIES: inspect the current Agent using read-only tools."})
                for attempt in range(600):
                    conversation = local("GET", f"/agents/{agent}/conversation")
                    candidates = [turn for turn in conversation.get("turns", [])
                        if "IOS_RICH_ACTIVITIES" in json.dumps(turn)]
                    if candidates and candidates[-1].get("brief_ids"):
                        rich_turn = candidates[-1]["turn_id"]
                        break
                    time.sleep(.1)
                else:
                    raise RuntimeError("rich tool turn did not complete with a real brief")
                activities = local("GET", f"/agents/{agent}/turns/{rich_turn}/activities?limit=60")
                if FakeProvider.rich_batches != 10 or not activities.get("has_more"):
                    raise RuntimeError("rich acceptance requires more than sixty real activities")
                print("Rich fixture: 91 Agents, 56 WorkItems, 60 real tool calls, large UTF-8/code, PNG and two-page PDF", flush=True)
            elif rich_acceptance:
                print("Rich file fixture: 80 directory notes, large UTF-8/code, PNG and two-page PDF", flush=True)
            # The active output probe belongs to native acceptance, not the
            # potentially long catalog/history preparation phase.
            task = local("POST", f"/control/agents/{agent}/tasks", {
                "summary": "IOS_POPULATED_TASK",
                "cmd": "printf 'IOS_POPULATED_OUTPUT\\n'; while :; do sleep 30; done",
                "workdir": str(root), "login": False, "yield_time_ms": 1})
            task_id = task["id"]
            test_env = dict(os.environ)
            test_env.update(HOLON_UI_BASE_URL=base, HOLON_UI_PAIRING_TICKET=pairing["ticket"],
                HOLON_UI_AGENT_ID=agent, HOLON_UI_WORK_ID=work_id, HOLON_UI_TASK_ID=task["id"],
                HOLON_UI_FILE_PATH=file_path, HOLON_UI_WORKSPACE_ID=workspace_id,
                HOLON_UI_BRIEF_MARKER="IOS_POPULATED_BRIEF")
            if rich_turn:
                test_env["HOLON_UI_RICH_TURN_ID"] = rich_turn
            print("真实隔离 populated daemon 已就绪；临时凭据不输出", flush=True)
            result = subprocess.run(["swift", "test", "--package-path", repo + "/packages/client-sdk-swift",
                                     "--filter", "LiveDaemonPopulatedProbeTests"], env=test_env)
            if result.returncode:
                raise RuntimeError("真实 populated SDK probes 失败")
            if mode != "--sdk-only":
                runner_inputs = {
                    "ENDPOINT": base,
                    "AGENT_ID": agent, "WORK_ID": work_id, "TASK_ID": task["id"], "WORKSPACE_ID": workspace_id,
                    "READ_MARKER": "IOS_POPULATED_BRIEF", "PLAN_MARKER": "IOS_POPULATED_FULL_PLAN",
                    "TASK_MARKER": "IOS_POPULATED_OUTPUT",
                    "FILE_REFERENCE": str(workspace / file_path), "FILE_MARKER": "IOS_POPULATED_FILE"}
                if proxy:
                    runner_inputs.update(ENDPOINT=f"http://127.0.0.1:{proxy.server_port}/api",
                        LOSS_CONTROL_URL=f"http://127.0.0.1:{proxy.server_port}/fixture/release-lost-response",
                        LOSS_CONTROL_TOKEN=proxy_control_token)
                if rich_turn:
                    runner_inputs.update(RICH_TURN_ID=rich_turn)
                    runner_inputs["RICH_ADDITIONAL_WORK_ID"] = rich_additional_work
                if rich_acceptance:
                    runner_inputs["RICH_DIRECTORY"] = "rich-files"
                for key, value in runner_inputs.items():
                    test_env["TEST_RUNNER_HOLON_UI_" + key] = value
                simulator = os.environ.get("IOS_SIMULATOR_ID")
                if not simulator:
                    raise RuntimeError("IOS_SIMULATOR_ID must identify a dedicated fresh iOS 18+ simulator")
                import re
                if not re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", simulator):
                    raise RuntimeError("IOS_SIMULATOR_ID must be a simulator UUID")
                destination = "platform=iOS Simulator,id=" + simulator
                derived = os.environ.get("IOS_DERIVED_DATA_PATH", str(root / "DerivedData"))
                if not pathlib.Path(derived).is_absolute() or pathlib.Path(derived) == pathlib.Path("/"):
                    raise RuntimeError("IOS_DERIVED_DATA_PATH must be a non-root absolute path")
                bundle = pathlib.Path(os.environ.get("IOS_RESULT_BUNDLE_PATH", str(root / "UI.xcresult")))
                if not bundle.is_absolute() or bundle == pathlib.Path("/") or bundle.exists():
                    raise RuntimeError("IOS_RESULT_BUNDLE_PATH must be an unused non-root absolute path")
                # Disconnected cases must run before authenticated installs credentials.
                cases = [("testDisconnectedEnglishLight", "large"),
                         ("testDisconnectedChineseDarkAccessibilitySize", MAXIMUM_TEXT_SIZE),
                         ("testPairingPreviewStaysOfflineAndCanCancel", "large"),
                         # Authenticate through shipped onboarding before diagnostics.
                         ("testAuthenticatedNativeWorkflow", "large"),
                         ("testChineseDiagnosticsDarkAccessibilitySize", MAXIMUM_TEXT_SIZE),
                         ("testDiagnosticsControlsRespondToRuntimeTextSize", MAXIMUM_TEXT_SIZE),
                         ("testPreparedDiagnosticsRespondToRuntimeTextSize", "large"),
                         ("testPreparedDiagnosticsViewportCoverage", MAXIMUM_TEXT_SIZE)]
                if rich_acceptance:
                    cases.extend([("testRichActivityWorkflow", "large"), ("testRichFilesWorkflow", "large"),
                                  ("testPopulatedComposerMaximumTextSize", MAXIMUM_TEXT_SIZE)])
                if lost_response_acceptance:
                    cases.append(("testLostResponseAndProcessRecovery", "large"))
                if history_acceptance:
                    cases.append(("testConversationHistoryWindowPosition", "large"))
                if share_acceptance:
                    cases.append(("testDirectAgentShareWorkflow", "large"))
                # Deletes the saved fixture session, so run last in a full sweep.
                cases.append(("testNetworkManagementWorkflow", "large"))
                selected_cases = os.environ.get("IOS_UI_CASES")
                if selected_cases:
                    requested = selected_cases.split(",")
                    if not requested or len(set(requested)) != len(requested) or set(requested) - {method for method, _ in cases}:
                        raise RuntimeError("IOS_UI_CASES must select known unique UI methods")
                    cases = [(method, size) for method, size in cases if method in requested]
                bundles = [bundle.with_name(bundle.stem + "-" + method + ".xcresult")
                           for method, _ in cases]
                if any(path.exists() for path in bundles):
                    raise RuntimeError("Per-case xcresult paths must be unused")
                initialize_simulator_text_size(simulator)
                if share_acceptance:
                    # The preceding xcodebuild may leave this simulator shut down.
                    from ios_share_probe import install
                    install(simulator, repo, root)
                for (method, content_size), case_bundle in zip(cases, bundles):
                    appearance = "dark" if method in {
                        "testDisconnectedChineseDarkAccessibilitySize",
                        "testChineseDiagnosticsDarkAccessibilitySize",
                        "testPopulatedComposerMaximumTextSize",
                    } else "light"
                    with simulator_text_size(simulator, content_size), simulator_appearance(simulator, appearance):
                        # Stop and drain the control service before restoring the case baseline.
                        with runtime_text_size_control(simulator) as (control_url, control_token):
                            test_env["TEST_RUNNER_HOLON_UI_TEXT_SIZE_URL"] = control_url
                            test_env["TEST_RUNNER_HOLON_UI_TEXT_SIZE_TOKEN"] = control_token
                            test_env["TEST_RUNNER_HOLON_UI_CONTENT_SIZE"] = content_size
                            test_env["TEST_RUNNER_HOLON_UI_APPEARANCE"] = appearance
                            if method in {"testAuthenticatedNativeWorkflow", "testDirectAgentShareWorkflow", "testNetworkManagementWorkflow"}:
                                # Tickets expire after two minutes. The preceding cases also
                                # warm the build; issue only when redemption is about to run.
                                ticket = local("POST", "/auth/pairing/issue")["ticket"]
                                test_env["TEST_RUNNER_HOLON_UI_PAIRING_CODE"] = ticket
                            if method == "testConversationHistoryWindowPosition":
                                history = local("GET", f"/agents/{agent}/conversation")
                                turns = sorted(history["turns"], key=lambda turn: turn["key"]["turn_index"])
                                if len(turns) <= 20:
                                    raise RuntimeError("native history acceptance needs more than one window")
                                older_end = max(20, len(turns) - 15)
                                test_env["TEST_RUNNER_HOLON_UI_HISTORY_OLDER_TOP"] = turns[older_end - 20]["turn_id"]
                                test_env["TEST_RUNNER_HOLON_UI_HISTORY_NEWER_TOP"] = turns[len(turns) - 20]["turn_id"]
                            result = subprocess.run(["xcodebuild", "-project", repo + "/apps/ios/Holon.xcodeproj",
                                "-scheme", "Holon", "-destination", destination,
                                "-parallel-testing-enabled", "NO",
                                # Keep test failures and attachments, but avoid a blocking sysdiagnose.
                                "-collect-test-diagnostics", "never",
                                "-test-timeouts-enabled", "YES",
                                "-default-test-execution-time-allowance", "600",
                                "-maximum-test-execution-time-allowance", "600",
                                "-derivedDataPath", derived,
                                "-only-testing:HolonUITests/HolonUITests/" + method,
                                "-resultBundlePath", str(case_bundle), "CODE_SIGN_IDENTITY=-", "test"], env=test_env)
                            if result.returncode:
                                raise RuntimeError(f"UI case {method} 失败（SDK 通过不能替代 UI）")
            else:
                print(f"{5 if rich_turn else 4} 项真实 SDK probes 已运行；SDK-only 未运行 UI", flush=True)
            if FakeProvider.image_requests:
                raise RuntimeError("Markdown renderer made an unconfirmed external image request")
            if share_acceptance and mode != "--sdk-only":
                conversation = local("GET", f"/agents/{agent}/conversation?limit=60")
                inputs = [value for turn in conversation["turns"] + conversation.get("active_turns", [])
                          for value in turn.get("inputs", [])] + conversation.get("pending_inputs", [])
                previews = json.dumps(inputs, ensure_ascii=False)
                for marker in ["IOS_SHARED_TEXT", "https://example.test/holon-share", "shared-note"]:
                    if marker not in previews:
                        raise RuntimeError("Direct-share input missing from authoritative conversation: " + marker)
                received_files = [path.read_bytes() for path in workspace.rglob("request-*") if path.is_file()]
                if b"IOS_SHARED_FILE_BYTES" not in received_files:
                    raise RuntimeError("Direct-share file bytes were not materialized by the daemon")
                if not any(data.startswith(b"\x89PNG\r\n\x1a\n") and len(data) >= 24 and
                           struct.unpack(">II", data[16:24]) == (16, 16) for data in received_files):
                    raise RuntimeError("Direct-share image bytes were not materialized by the daemon")
                print("Direct-share acceptance: OS extension text, URL, image and file received by isolated Agent", flush=True)
            if lost_response_acceptance and mode != "--sdk-only":
                if len(lost_receipts) < 2 or len({request for request, _ in lost_receipts}) != 1:
                    raise RuntimeError("response loss retry must retain one immutable UUID")
                if len({receipt["message_id"] for _, receipt in lost_receipts}) != 1:
                    raise RuntimeError("retry enqueued a duplicate message")
                if lost_receipts[0][1]["disposition"] != "accepted" or lost_receipts[-1][1]["disposition"] != "duplicate":
                    raise RuntimeError("retry must receive the real daemon's duplicate receipt")
                print("Lost-response acceptance: immutable UUID and one real message ID across explicit retry", flush=True)
        finally:
            try:
                if task_id is not None:
                    local("POST", f"/control/agents/{agent}/tasks/{task_id}/stop", {})
            finally:
                release_held_run.set()
                if proxy:
                    proxy.shutdown(); proxy.server_close()
                provider.shutdown()
                provider.server_close()
                daemon.terminate()
                try:
                    daemon.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()
