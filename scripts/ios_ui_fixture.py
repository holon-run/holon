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

from ios_simulator_text_size import (
    MAXIMUM_TEXT_SIZE, initialize_simulator_text_size, runtime_text_size_control,
    simulator_text_size,
)

binary, repo, mode = sys.argv[1:]
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
            text = "IOS_POPULATED_BRIEF: isolated iOS contract fixture reply."
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

            work = local("POST", "/control/agents/main/work-items", {"objective": "IOS_POPULATED_WORK"})
            work_id = work["id"]
            plan = pathlib.Path(work["plan_artifact"]["path"])
            if not plan.resolve().is_relative_to(root.resolve()):
                raise RuntimeError("plan escapes fixture root")
            plan.write_text("# IOS_POPULATED_PLAN\n\n" + "Real isolated work plan.\n" * 500
                            + "\nIOS_POPULATED_FULL_PLAN\n")
            local("POST", "/agents/main/enqueue", {"text": "IOS_POPULATED_BRIEF: Produce the isolated fixture reading brief."})
            for attempt in range(150):
                conversation = local("GET", "/agents/main/conversation")
                if "IOS_POPULATED_BRIEF" in json.dumps(conversation):
                    break
                time.sleep(.1)
            else:
                raise RuntimeError("real conversation brief timed out")
            # The deterministic provider returns a real brief; wait for the run to settle.
            for attempt in range(150):
                try:
                    completed = local("POST", f"/control/agents/main/work-items/{work_id}/complete",
                                      {"report_text": "IOS_POPULATED_BRIEF: isolated work completed."})
                    break
                except RuntimeError as error:
                    if "active" not in str(error) and "409" not in str(error):
                        raise
                    time.sleep(.1)
            else:
                raise RuntimeError("work completion timed out")
            file_path = "ios-populated.txt"
            workspace = root / "holon" / "agents" / "main"
            (workspace / file_path).write_text("IOS_POPULATED_FILE\n")
            task = local("POST", "/control/agents/main/tasks", {
                "summary": "IOS_POPULATED_TASK",
                "cmd": "printf 'IOS_POPULATED_OUTPUT\\n'; while :; do sleep 30; done",
                "workdir": str(root), "login": False, "yield_time_ms": 1})
            task_id = task["id"]
            state = local("GET", "/agents/main/state")
            workspace_id = next(w["workspace_id"] for w in state["workspace"]["workspaces"]
                                if w.get("kind") == "agent_home" or w.get("workspace_id", "").startswith("agent_home"))
            test_env = dict(os.environ)
            test_env.update(HOLON_UI_BASE_URL=base, HOLON_UI_PAIRING_TICKET=pairing["ticket"],
                HOLON_UI_AGENT_ID="main", HOLON_UI_WORK_ID=work_id, HOLON_UI_TASK_ID=task["id"],
                HOLON_UI_FILE_PATH=file_path, HOLON_UI_WORKSPACE_ID=workspace_id,
                HOLON_UI_BRIEF_MARKER="IOS_POPULATED_BRIEF")
            print("真实隔离 populated daemon 已就绪；临时凭据不输出", flush=True)
            result = subprocess.run(["swift", "test", "--package-path", repo + "/packages/client-sdk-swift",
                                     "--filter", "LiveDaemonPopulatedProbeTests"], env=test_env)
            if result.returncode:
                raise RuntimeError("真实 populated SDK probes 失败")
            if mode != "--sdk-only":
                runner_inputs = {
                    "ENDPOINT": base,
                    "AGENT_ID": "main", "WORK_ID": work_id, "TASK_ID": task["id"],
                    "READ_MARKER": "IOS_POPULATED_BRIEF", "PLAN_MARKER": "IOS_POPULATED_FULL_PLAN",
                    "TASK_MARKER": "IOS_POPULATED_OUTPUT",
                    "FILE_REFERENCE": str(workspace / file_path), "FILE_MARKER": "IOS_POPULATED_FILE"}
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
                         ("testDiagnosticsControlsRespondToRuntimeTextSize", MAXIMUM_TEXT_SIZE),
                         ("testPreparedDiagnosticsRespondToRuntimeTextSize", "large"),
                         ("testPreparedDiagnosticsViewportCoverage", MAXIMUM_TEXT_SIZE)]
                bundles = [bundle.with_name(bundle.stem + "-" + method + ".xcresult")
                           for method, _ in cases]
                if any(path.exists() for path in bundles):
                    raise RuntimeError("Per-case xcresult paths must be unused")
                initialize_simulator_text_size(simulator)
                for (method, content_size), case_bundle in zip(cases, bundles):
                    with simulator_text_size(simulator, content_size):
                        # Stop and drain the control service before restoring the case baseline.
                        with runtime_text_size_control(simulator) as (control_url, control_token):
                            test_env["TEST_RUNNER_HOLON_UI_TEXT_SIZE_URL"] = control_url
                            test_env["TEST_RUNNER_HOLON_UI_TEXT_SIZE_TOKEN"] = control_token
                            test_env["TEST_RUNNER_HOLON_UI_CONTENT_SIZE"] = content_size
                            if method == "testAuthenticatedNativeWorkflow":
                                # Tickets expire after two minutes. The preceding cases also
                                # warm the build; issue only when redemption is about to run.
                                ticket = local("POST", "/auth/pairing/issue")["ticket"]
                                test_env["TEST_RUNNER_HOLON_UI_PAIRING_CODE"] = ticket
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
                print("4 项真实 SDK probes 已运行；SDK-only 未运行 UI", flush=True)
        finally:
            try:
                if task_id is not None:
                    local("POST", f"/control/agents/main/tasks/{task_id}/stop", {})
            finally:
                release_held_run.set()
                provider.shutdown()
                provider.server_close()
                daemon.terminate()
                try:
                    daemon.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()
