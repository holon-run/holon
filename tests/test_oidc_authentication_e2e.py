#!/usr/bin/env python3
"""Black-box OIDC acceptance against an isolated, freshly built Holon binary.

See docs/testing/oidc-authentication-case.md for the build/run command.
Only Python's standard library and the openssl executable are required.
"""

import base64
import hashlib
import http.cookies
import http.server
import json
import os
from pathlib import Path
import secrets
import socket
import ssl
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor


CLIENT_ID = "holon-oidc-e2e"
BINARY = os.environ.get("HOLON_OIDC_E2E_BINARY")


def base64url(value):
    return base64.urlsafe_b64encode(value).decode().rstrip("=")


def challenge(verifier):
    return base64url(hashlib.sha256(verifier.encode()).digest())


def openssl(*args, data=None):
    return subprocess.run(
        ["openssl", *map(str, args)],
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
        timeout=20,
    ).stdout


def certificates(directory):
    config = directory / "openssl.cnf"
    config.write_text(
        "[req]\ndistinguished_name=dn\n[dn]\n"
        "[ca]\nbasicConstraints=critical,CA:TRUE\n"
        "keyUsage=critical,keyCertSign,cRLSign\n"
        "[leaf]\nbasicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\n"
        "subjectAltName=DNS:localhost,IP:127.0.0.1\n"
    )
    ca_key, ca = directory / "ca.key", directory / "ca.pem"
    key, csr, cert = (directory / name for name in ("server.key", "server.csr", "server.pem"))
    openssl(
        "req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-days", "1", "-subj", "/CN=Holon OIDC test CA",
        "-keyout", ca_key, "-out", ca, "-config", config, "-extensions", "ca",
    )
    openssl(
        "req", "-new", "-newkey", "rsa:2048", "-nodes",
        "-subj", "/CN=localhost", "-keyout", key, "-out", csr,
    )
    openssl(
        "x509", "-req", "-in", csr, "-CA", ca, "-CAkey", ca_key,
        "-set_serial", "1", "-days", "1", "-out", cert,
        "-extfile", config, "-extensions", "leaf",
    )
    return ca, key, cert


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return None


class MockIdp:
    def __init__(self, key, cert):
        self.key = key
        self.codes = {}
        self.calls = {}
        self.lock = threading.Lock()
        modulus = openssl("rsa", "-in", key, "-noout", "-modulus").decode().strip().split("=")[1]
        self.jwk = {
            "kty": "RSA", "kid": "e2e", "alg": "RS256", "use": "sig",
            "n": base64url(bytes.fromhex(modulus)), "e": "AQAB",
        }
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass  # Never log authorization codes, tokens, or query strings.

            def reply(self, status, body=None, location=None):
                encoded = json.dumps(body).encode() if body is not None else b""
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                if location is not None:
                    self.send_header("Location", location)
                self.end_headers()
                self.wfile.write(encoded)

            def do_GET(self):
                parsed = urllib.parse.urlsplit(self.path)
                fixture.record(parsed.path)
                query = dict(urllib.parse.parse_qsl(parsed.query))
                if parsed.path == "/.well-known/openid-configuration":
                    self.reply(200, {
                        "issuer": fixture.issuer,
                        "authorization_endpoint": fixture.issuer + "/authorize",
                        "token_endpoint": fixture.issuer + "/token",
                        "jwks_uri": fixture.issuer + "/jwks",
                        "response_types_supported": ["code"],
                        "subject_types_supported": ["public"],
                        "id_token_signing_alg_values_supported": ["RS256"],
                        "code_challenge_methods_supported": ["S256"],
                    })
                elif parsed.path == "/jwks":
                    self.reply(200, {"keys": [fixture.jwk]})
                elif parsed.path == "/authorize":
                    required = ["state", "nonce", "code_challenge"]
                    if (
                        query.get("client_id") != CLIENT_ID
                        or query.get("response_type") != "code"
                        or query.get("redirect_uri") != fixture.callback
                        or query.get("code_challenge_method") != "S256"
                        or not all(query.get(name) for name in required)
                    ):
                        self.reply(400, {"error": "invalid_authorization_request"})
                        return
                    code = secrets.token_urlsafe(32)
                    with fixture.lock:
                        fixture.codes[code] = query
                    self.reply(302, location=fixture.callback + "?" + urllib.parse.urlencode({
                        "state": query["state"], "code": code,
                    }))
                else:
                    self.reply(404, {"error": "not_found"})

            def do_POST(self):
                fixture.record("/token")
                if self.path != "/token":
                    self.reply(404, {"error": "not_found"})
                    return
                body = self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode()
                form = dict(urllib.parse.parse_qsl(body))
                with fixture.lock:
                    authorization = fixture.codes.pop(form.get("code"), None)
                if (
                    authorization is None
                    or form.get("grant_type") != "authorization_code"
                    or form.get("client_id") != CLIENT_ID
                    or form.get("redirect_uri") != fixture.callback
                    or challenge(form.get("code_verifier", "")) != authorization["code_challenge"]
                ):
                    self.reply(400, {"error": "invalid_grant"})
                    return
                self.reply(200, {
                    "id_token": fixture.id_token(authorization),
                    "access_token": secrets.token_urlsafe(32),
                    "token_type": "Bearer", "expires_in": 300,
                })

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.issuer = f"https://localhost:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def record(self, path):
        with self.lock:
            self.calls[path] = self.calls.get(path, 0) + 1

    def id_token(self, authorization):
        now = int(time.time())
        claims = {
            "iss": self.issuer, "sub": authorization.get("test_user", "alice"),
            "aud": CLIENT_ID, "iat": now, "exp": now + 300,
            "nonce": authorization["nonce"], "name": "OIDC fixture user",
        }
        mode = authorization.get("test_claim", "")
        if mode == "issuer":
            claims["iss"] = self.issuer + "/wrong"
        elif mode == "audience":
            claims["aud"] = "another-client"
        elif mode == "nonce":
            claims["nonce"] = "wrong-nonce"
        elif mode == "missing_nonce":
            claims.pop("nonce")
        elif mode == "expired":
            claims["exp"] = now - 600
        elif mode == "not_yet_valid":
            claims["nbf"] = now + 600
        elif mode == "future_iat":
            claims["iat"] = now + 600
        signing_input = ".".join(base64url(json.dumps(value).encode()) for value in [
            {"alg": "RS256", "kid": "unknown" if mode == "kid" else "e2e"}, claims,
        ]).encode()
        signature = openssl("dgst", "-sha256", "-sign", self.key, data=signing_input)
        if mode == "signature":
            signature = bytes([signature[0] ^ 1]) + signature[1:]
        return signing_input.decode() + "." + base64url(signature)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=10)


@unittest.skipUnless(BINARY, "set HOLON_OIDC_E2E_BINARY to the native-roots test build")
class OidcAuthenticationE2E(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.process = None
        cls.idp = None
        cls.log = None
        cls.temp = tempfile.TemporaryDirectory(prefix="oidc-")
        cls.addClassCleanup(cls.close)
        cls.root = Path(cls.temp.name)
        ca, key, cert = certificates(cls.root)
        cls.idp = MockIdp(key, cert)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        cls.base = f"http://localhost:{port}"
        cls.idp.callback = cls.base + "/api/auth/oidc/callback"
        cls.home = cls.root / "holon"
        cls.home.mkdir()
        (cls.root / "workspace").mkdir()
        (cls.root / "user").mkdir()
        (cls.home / "config.json").write_text(json.dumps({
            "auth": {
                "mode": "oidc",
                "oidc": {
                    "issuer_url": cls.idp.issuer, "client_id": CLIENT_ID,
                    "redirect_uri": cls.idp.callback,
                },
                "session": {"absolute_ttl_seconds": 300, "idle_ttl_seconds": 300},
            },
        }))
        cls.environment = dict(os.environ)
        cls.environment.update({
            "HOME": str(cls.root / "user"), "HOLON_HOME": str(cls.home),
            "HOLON_WORKSPACE_DIR": str(cls.root / "workspace"),
            "HOLON_SOCKET_PATH": str(cls.home / "run" / "holon.sock"),
            "HOLON_HTTP_ADDR": f"127.0.0.1:{port}", "HOLON_CALLBACK_BASE_URL": cls.base,
            "HOLON_BOOTSTRAP": "1", "SSL_CERT_FILE": str(ca), "SSL_CERT_DIR": str(cls.root),
            "NO_PROXY": "localhost,127.0.0.1", "RUST_LOG": "warn",
        })
        cls.log = (cls.root / "holon.log").open("wb")
        cls.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect(),
            urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=str(ca))),
        )
        cls.process = subprocess.Popen(
            [str(Path(BINARY).resolve()), "serve", "--host", "127.0.0.1", "--port", str(port)],
            cwd=cls.root / "workspace", env=cls.environment,
            stdout=cls.log, stderr=subprocess.STDOUT,
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if cls.process.poll() is not None:
                raise RuntimeError(f"isolated Holon exited during startup ({cls.process.returncode})")
            try:
                if cls.request("/api/auth/method")[0] == 200:
                    return
            except urllib.error.URLError:
                pass
            time.sleep(0.1)
        raise RuntimeError("isolated Holon did not become ready within 30 seconds")

    @classmethod
    def close(cls):
        if cls.process is not None:
            cls.process.terminate()
            try:
                cls.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                cls.process.kill()
                cls.process.wait(timeout=10)
        if cls.idp is not None:
            cls.idp.close()
        if cls.log is not None:
            cls.log.close()
        cls.temp.cleanup()

    @classmethod
    def request(cls, path, data=None, credential=None, cookie=None):
        url = path if path.startswith(("http://", "https://")) else cls.base + path
        headers = {}
        if credential:
            headers["Authorization"] = "Bearer " + credential
        if cookie:
            headers["Cookie"] = cookie
        encoded = None
        if data is not None:
            encoded = json.dumps(data).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=encoded, headers=headers)
        try:
            response = cls.opener.open(request, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            return response.code, response.headers, response.read()

    def status(self, response, expected):
        # Do not include URLs, headers, or credentials in assertion output.
        self.assertEqual(response[0], expected, "unexpected HTTP status")
        return response

    def cookie(self, response, name):
        cookies = http.cookies.SimpleCookie()
        for value in response[1].get_all("Set-Cookie", []):
            cookies.load(value)
        self.assertIn(name, cookies)
        return name + "=" + cookies[name].value

    def authorize(self, user="alice", claim="", native=True):
        verifier = secrets.token_urlsafe(48)
        state = secrets.token_urlsafe(24)
        path = "/api/auth/oidc/start"
        if native:
            path = "/api/auth/oidc/native/start?" + urllib.parse.urlencode({
                "state": state, "code_challenge": challenge(verifier),
                "code_challenge_method": "S256",
            })
        start = self.status(self.request(path), 302)
        browser_cookie = self.cookie(start, "holon_oidc_state")
        authorization = urllib.parse.urlsplit(start[1]["Location"])
        query = dict(urllib.parse.parse_qsl(authorization.query))
        self.assertEqual(query["code_challenge_method"], "S256")
        self.assertTrue(query["nonce"])
        self.assertEqual(query["redirect_uri"], self.idp.callback)
        query.update(test_user=user, test_claim=claim)
        provider = self.status(self.request(authorization._replace(
            query=urllib.parse.urlencode(query),
        ).geturl()), 302)
        callback = provider[1]["Location"]
        return {
            "callback": callback, "cookie": browser_cookie,
            "verifier": verifier, "state": state,
        }

    def native_ticket(self, user="alice"):
        login = self.authorize(user=user)
        response = self.status(self.request(login["callback"], cookie=login["cookie"]), 302)
        redirect = urllib.parse.urlsplit(response[1]["Location"])
        self.assertEqual(
            (redirect.scheme, redirect.netloc, redirect.path),
            ("run.holon.android", "oidc", "/callback"),
        )
        query = dict(urllib.parse.parse_qsl(redirect.query))
        self.assertEqual(set(query), {"state", "ticket", "code_challenge_method"})
        self.assertEqual(query["state"], login["state"])
        self.assertEqual(query["code_challenge_method"], "S256")
        self.assertNotIn("holon_session", str(response[1].get_all("Set-Cookie", [])))
        return query["ticket"], login["verifier"]

    def exchange(self, ticket, verifier=None, native=True):
        body = {"credential": ticket}
        if verifier is not None:
            body["native_verifier"] = verifier
        suffix = "/native" if native else ""
        return self.request("/api/auth/session/exchange" + suffix, data=body)

    def native_session(self, user="alice"):
        ticket, verifier = self.native_ticket(user=user)
        response = self.status(self.exchange(ticket, verifier), 200)
        session = json.loads(response[2])
        self.assertTrue(session["ok"])
        self.assertTrue(session["credential"])
        return session, self.cookie(response, "holon_session")

    def test_native_login_bearer_cookie_and_logout(self):
        self.status(self.request("/api/auth/session/me"), 401)
        session, cookie = self.native_session()
        for authentication in [{"credential": session["credential"]}, {"cookie": cookie}]:
            me = self.status(self.request("/api/auth/session/me", **authentication), 200)
            self.assertEqual(json.loads(me[2])["user_id"], session["user_id"])
        self.status(self.request(
            "/api/auth/session/logout", data={}, credential=session["credential"],
        ), 204)
        for authentication in [{"credential": session["credential"]}, {"cookie": cookie}]:
            self.status(self.request("/api/auth/session/me", **authentication), 401)
        for endpoint in ["/.well-known/openid-configuration", "/authorize", "/token", "/jwks"]:
            self.assertGreater(self.idp.calls.get(endpoint, 0), 0)

    def test_interception_does_not_redeem_or_burn_ticket(self):
        ticket, verifier = self.native_ticket()
        self.status(self.exchange(ticket), 401)
        self.status(self.exchange(ticket, secrets.token_urlsafe(48)), 401)
        self.status(self.exchange(ticket, native=False), 401)
        self.status(self.exchange(ticket, verifier, native=False), 401)
        self.status(self.exchange(ticket, verifier), 200)
        self.status(self.exchange(ticket, verifier), 401)

    def test_concurrent_exchange_has_one_winner(self):
        ticket, verifier = self.native_ticket()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: self.exchange(ticket, verifier)[0], range(2)))
        self.assertEqual(sorted(results), [200, 401])

    def test_rejects_plain_and_missing_pkce(self):
        discovery_calls = self.idp.calls.get("/.well-known/openid-configuration", 0)
        for method in ["plain", "s256", "unknown"]:
            with self.subTest(method=method):
                query = urllib.parse.urlencode({
                    "state": "state", "code_challenge": challenge("v" * 64),
                    "code_challenge_method": method,
                })
                self.status(self.request("/api/auth/oidc/native/start?" + query), 400)
        self.status(self.request("/api/auth/oidc/native/start?state=state"), 400)
        self.assertEqual(self.idp.calls.get("/.well-known/openid-configuration", 0), discovery_calls)

    def test_browser_login_and_state_binding(self):
        login = self.authorize(native=False)
        self.assertGreaterEqual(self.request(login["callback"])[0], 400)
        response = self.status(self.request(login["callback"], cookie=login["cookie"]), 302)
        cookie = self.cookie(response, "holon_session")
        self.status(self.request("/api/auth/session/me", cookie=cookie), 200)
        self.assertGreaterEqual(self.request(login["callback"], cookie=login["cookie"])[0], 400)

    def test_rejects_invalid_signed_id_token_claims(self):
        for claim in [
            "issuer", "audience", "nonce", "missing_nonce", "expired",
            "not_yet_valid", "future_iat", "signature", "kid",
        ]:
            with self.subTest(claim=claim):
                login = self.authorize(claim=claim)
                response = self.request(login["callback"], cookie=login["cookie"])
                self.assertGreaterEqual(response[0], 400)
                self.assertNotIn("Location", response[1])
                self.assertNotIn("holon_session", str(response[1].get_all("Set-Cookie", [])))

    def test_scopes_match_and_caches_do_not_cross_users(self):
        alice, _ = self.native_session("scope-alice")
        self.status(self.request(
            "/api/control/agents/oidc-e2e/create", data={}, credential=alice["credential"],
        ), 200)
        bob, _ = self.native_session("scope-bob")
        rotated_alice, _ = self.native_session("scope-alice")
        self.assertEqual(alice["user_id"], rotated_alice["user_id"])
        self.assertNotEqual(alice["user_id"], bob["user_id"])
        scopes = []
        for session in [alice, bob, rotated_alice, bob, alice]:
            credential = session["credential"]
            roster = json.loads(self.status(self.request(
                "/api/agents/snapshot", credential=credential,
            ), 200)[2])
            projection = json.loads(self.status(self.request(
                "/api/agents/oidc-e2e/projection-snapshot", credential=credential,
            ), 200)[2])
            reads = json.loads(self.status(self.request(
                "/api/agents/brief-read-states", credential=credential,
            ), 200)[2])
            read = next(value for value in reads if value["agent_id"] == "oidc-e2e")
            scope = roster["visibility_scope_id"]
            self.assertEqual(scope, projection["visibility_scope_id"])
            self.assertEqual(scope, read["visibility_scope_id"])
            for endpoint in [
                "/api/agents/oidc-e2e/conversation",
                "/api/control/agents/oidc-e2e/conversation/shadow-diagnostics",
            ]:
                conversation = json.loads(self.status(self.request(
                    endpoint, credential=credential,
                ), 200)[2])
                self.assertEqual(scope, conversation["visibility_scope_id"])
            scopes.append(scope)
        self.assertEqual(scopes[0], scopes[2])
        self.assertEqual(scopes[0], scopes[4])
        self.assertEqual(scopes[1], scopes[3])
        self.assertNotEqual(scopes[0], scopes[1])


if __name__ == "__main__":
    unittest.main()
