import Foundation
import XCTest
@testable import HolonMenu

actor RecordingProcessLauncher: HolonProcessLaunching {
    struct Invocation: Equatable, Sendable {
        let executableURL: URL
        let arguments: [String]
    }

    private var recordedInvocations: [Invocation] = []
    private var result: Result<HolonProcessResult, Error>
    private let address: String?
    private let responses: [String: String]

    init(
        result: Result<HolonProcessResult, Error>,
        address: String? = nil,
        responses: [String: String] = [:]
    ) {
        self.result = result
        self.address = address
        self.responses = responses
    }

    func run(executableURL: URL, arguments: [String]) async throws -> HolonProcessResult {
        recordedInvocations.append(Invocation(executableURL: executableURL, arguments: arguments))
        if arguments.first == "getifaddr", let address {
            return HolonProcessResult(
                terminationStatus: 0, stdout: Data(address.utf8), stderr: Data()
            )
        }
        if let response = responses[arguments.joined(separator: " ")] {
            return HolonProcessResult(
                terminationStatus: 0, stdout: Data(response.utf8), stderr: Data()
            )
        }
        return try result.get()
    }

    func invocations() -> [Invocation] {
        recordedInvocations
    }

    func replaceResult(_ result: HolonProcessResult) {
        self.result = .success(result)
    }
}

final class HolonMenuClientTests: XCTestCase {
    func testTokenlessPairingUsesTrustedUnixControlSocket() async throws {
        let curlArguments = ["--silent", "--show-error", "--fail", "--unix-socket",
                             "/tmp/holon.sock", "--request", "POST",
                             "http://localhost/api/auth/pairing/issue"]
        let launcher = RecordingProcessLauncher(
            result: .success(HolonProcessResult(
                terminationStatus: 0,
                stdout: Data("""
                    {"ok":true,"state":"running","healthy":true,"home_dir":"/tmp/holon",
                    "socket_path":"/tmp/holon.sock","http_addr":"127.0.0.1:7878",
                    "web_url":"http://127.0.0.1:7878","desired_running":true,
                    "control_connectivity":true,"message":"Running"}
                    """.utf8),
                stderr: Data()
            )),
            responses: [curlArguments.joined(separator: " "): #"{"ticket":"one-time-code"}"#]
        )
        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher,
            launchOptions: HolonDaemonLaunchOptions(tokenFilePath: "/tmp/nonexistent-holon-menu-token"),
            networkSession: serveSession()
        )

        let url = try await client.authenticatedWebURL()
        XCTAssertEqual(url.absoluteString, "http://127.0.0.1:7878/login#pair=one-time-code")
        let invocations = await launcher.invocations()
        XCTAssertEqual(invocations.last?.executableURL.path, "/usr/bin/curl")
        XCTAssertEqual(invocations.last?.arguments, curlArguments)
    }

    private final class ServeURLProtocol: URLProtocol {
        static let lock = NSLock()
        nonisolated(unsafe) static var requests: [(String, String, String?, URL?)] = []
        nonisolated(unsafe) static var responseCode = 200
        nonisolated(unsafe) static var authenticationAvailable: Bool? = true
        nonisolated(unsafe) static var authenticateBearer = false
        nonisolated(unsafe) static var authMode = "local"

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
        override func startLoading() {
            Self.lock.lock()
            Self.requests.append((
                request.httpMethod ?? "",
                request.url?.path ?? "",
                request.value(forHTTPHeaderField: "Authorization"),
                request.url
            ))
            let code = Self.responseCode
            let authMode = Self.authMode
            let authenticated = Self.authenticationAvailable.map {
                $0 || Self.authenticateBearer && request.value(forHTTPHeaderField: "Authorization") != nil
            }
            Self.lock.unlock()
            client?.urlProtocol(self, didReceive: HTTPURLResponse(
                url: request.url!, statusCode: code, httpVersion: nil, headerFields: nil
            )!, cacheStoragePolicy: .notAllowed)
            if request.url?.path == "/api/auth/method" {
                client?.urlProtocol(self, didLoad: Data("{\"mode\":\"\(authMode)\"}".utf8))
                client?.urlProtocolDidFinishLoading(self)
                return
            }
            client?.urlProtocol(self, didLoad: Data("""
                {"desired_enabled":true,"available":true,"connected":true,"status_known":true,
                 "serving":false,"conflict":false,"hostname":"holon.example.ts.net",
                 "serve_url":"https://holon.example.ts.net","message":"Not serving",
                 "control_authentication_available":\(authenticated.map(String.init) ?? "null")}
                """.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }
        override func stopLoading() {}
    }

    private func serveSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ServeURLProtocol.self]
        return URLSession(configuration: configuration)
    }

    func testOIDCConnectionUsesNormalLoginWithoutIssuingTicket() async throws {
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.authMode = "oidc"
            ServeURLProtocol.responseCode = 200
        }
        defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authMode = "local" } }
        let launcher = RecordingProcessLauncher(result: .success(HolonProcessResult(
            terminationStatus: 0,
            stdout: Data("""
                {"ok":true,"state":"running","healthy":true,"home_dir":"/tmp/holon",
                 "socket_path":"/tmp/holon.sock","http_addr":"127.0.0.1:7878",
                 "web_url":"http://127.0.0.1:7878","desired_running":true,
                 "control_connectivity":true,"message":"Running"}
                """.utf8),
            stderr: Data()
        )))
        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher, networkSession: serveSession()
        )
        let url = try await client.pairingURL(for: URL(string: "https://holon.example.com")!)
        XCTAssertEqual(url.absoluteString, "https://holon.example.com/login")
        let localURL = try await client.authenticatedWebURL()
        XCTAssertEqual(localURL.absoluteString, "http://127.0.0.1:7878/login")
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.executableURL.path == "/usr/bin/curl" })
    }

    private func serveClient(
        address: String = "127.0.0.1:7878",
        home: String = "/tmp/holon",
        token: String? = "test-token",
        daemonExecutable: String = "/opt/holon",
        lanAddress: String? = nil,
        desktopIntegration: Bool? = true,
        processCreated: Bool? = nil,
        preferences: UserDefaults = .standard
    ) -> (HolonCLIClient, RecordingProcessLauncher) {
        let launcher = RecordingProcessLauncher(result: .success(HolonProcessResult(
            terminationStatus: 0,
            stdout: Data("""
                {"ok":true,"state":"running","healthy":true,"home_dir":"\(home)",
                "socket_path":"/tmp/holon.sock","http_addr":"\(address)",
                "web_url":"http://127.0.0.1:7878","desired_running":true,
                "control_connectivity":true,"executable_path":"\(daemonExecutable)","pid":99,"message":"Running"
                \(processCreated.map { ",\"process_created\":\($0)" } ?? "")}
                """.utf8),
            stderr: Data()
        )), address: lanAddress)
        return (HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher,
            launchOptions: HolonDaemonLaunchOptions(token: token, desktopIntegration: desktopIntegration),
            networkSession: serveSession(),
            preferences: preferences
        ), launcher)
    }
    func testParsesConnectedAndServingTailscaleStatus() {
        let status = HolonTailscaleStatus.parse(
            statusOutput: #"{"BackendState":"Running","Self":{"DNSName":"holon.example.ts.net."}}"#,
            serveOutput: #"{"Web":{"holon.example.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:7878"},"/other":{"Proxy":"http://127.0.0.1:9000"}}}}}"#,
            holonURL: URL(string: "http://127.0.0.1:7878")
        )

        XCTAssertEqual(status.state, .serving)
        XCTAssertEqual(status.hostname, "holon.example.ts.net")
        XCTAssertEqual(status.serveURL?.absoluteString, "https://holon.example.ts.net")
    }

    func testIgnoresServeForAnotherService() {
        let status = HolonTailscaleStatus.parse(
            statusOutput: #"{"BackendState":"Running","Self":{"DNSName":"holon.example.ts.net."}}"#,
            serveOutput: #"{"Web":{"holon.example.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:9000"}}}}}"#,
            holonURL: URL(string: "http://127.0.0.1:7878")
        )
        XCTAssertEqual(status.state, .connected)
        XCTAssertNil(status.serveURL)
    }

    func testExistingNonProxyRootHandlerIsNotHolon() throws {
        let configuration = try JSONDecoder().decode(
            HolonTailscaleServeConfiguration.self,
            from: Data(#"{"Web":{"holon.example.ts.net:443":{"Handlers":{"/":{"Text":"Other service"}}}}}"#.utf8)
        )
        XCTAssertTrue(configuration.hasRootHandler(for: "holon.example.ts.net"))
        XCTAssertNil(configuration.rootProxy(for: "holon.example.ts.net"))
    }

    func testParsesTailscaleLoginRequirement() {
        let status = HolonTailscaleStatus.parse(
            statusOutput: #"{"BackendState":"NeedsLogin"}"#
        )

        XCTAssertEqual(status.state, .loggedOut)
    }

    func testClientBuildsDaemonArgumentsAndDecodesJSON() async throws {
        let statusJSON = """
        {
          "ok": true,
          "state": "running",
          "healthy": true,
          "home_dir": "/Users/jane/.holon",
          "socket_path": "/tmp/holon.sock",
          "http_addr": "127.0.0.1:7878",
          "web_url": "http://127.0.0.1:7878",
          "product_version": "0.49.0 (abcdef0)",
          "control_protocol_version": 1,
          "lifecycle_owner": "standalone",
          "executable_path": "/Applications/Holon.app/Contents/Resources/bin/holon",
          "desired_running": true,
          "pid": 123,
          "control_connectivity": true,
          "runtime_config_fingerprint": "abc123",
          "config_fingerprint_match": true,
          "message": "Holon runtime is running."
        }
        """

        let launcher = RecordingProcessLauncher(
            result: .success(
                HolonProcessResult(
                    terminationStatus: 0,
                    stdout: Data(statusJSON.utf8),
                    stderr: Data()
                )
            )
        )

        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher,
            launchOptions: HolonDaemonLaunchOptions(access: "local", port: 7878)
        )

        let status = try await client.status()
        XCTAssertEqual(status.state, .running)
        XCTAssertEqual(status.httpAddr, "127.0.0.1:7878")
        XCTAssertEqual(status.webUrl, "http://127.0.0.1:7878")
        XCTAssertEqual(status.webURL?.absoluteString, "http://127.0.0.1:7878")
        let statusInvocations = await launcher.invocations()
        XCTAssertEqual(statusInvocations.first?.arguments, ["daemon", "status"])

        _ = try await client.start()
        let startInvocations = await launcher.invocations()
        XCTAssertEqual(
            startInvocations.last?.arguments,
            ["daemon", "start", "--access", "local", "--port", "7878", "--desktop-integration"]
        )

        _ = try await client.restart()
        let restartInvocations = await launcher.invocations()
        XCTAssertEqual(
            restartInvocations.last?.arguments,
            ["daemon", "restart", "--access", "local", "--port", "7878", "--desktop-integration"]
        )
    }

    func testLANUsesWildcardListenerAndIndependentDesktopIntegration() async throws {
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.authenticationAvailable = false
            ServeURLProtocol.authenticateBearer = true
        }
        defer {
            ServeURLProtocol.lock.withLock {
                ServeURLProtocol.authenticationAvailable = true
                ServeURLProtocol.authenticateBearer = false
            }
        }
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let statusJSON = """
        {"ok":true,"state":"running","healthy":true,"home_dir":"\(home.path)",
        "socket_path":"/tmp/holon.sock","http_addr":"0.0.0.0:7878",
        "web_url":"http://127.0.0.1:7878","desired_running":true,
        "control_connectivity":true,"message":"Running"}
        """
        let launcher = RecordingProcessLauncher(
            result: .success(HolonProcessResult(
                terminationStatus: 0, stdout: Data(statusJSON.utf8), stderr: Data()
            )),
            address: "192.168.1.20\n"
        )
        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher,
            launchOptions: HolonDaemonLaunchOptions(access: "local", port: 7878),
            networkSession: serveSession()
        )
        let webURL = try await client.webURL()
        XCTAssertEqual(webURL.absoluteString, "http://192.168.1.20:7878")
        let addressLookup = await launcher.invocations().first {
            $0.arguments.first == "getifaddr"
        }
        XCTAssertEqual(addressLookup?.executableURL.path, "/usr/sbin/ipconfig")
        XCTAssertEqual(addressLookup?.arguments, ["getifaddr", "en0"])
        _ = try await client.enableLAN()
        _ = try await client.restart()
        var invocations = await launcher.invocations()
        let tokenPath = home.appendingPathComponent("control.token").path
        let token = try String(contentsOfFile: tokenPath, encoding: .utf8)
        XCTAssertEqual(token.count, 64)
        XCTAssertEqual(
            try FileManager.default.attributesOfItem(atPath: tokenPath)[.posixPermissions] as? Int,
            0o600
        )
        XCTAssertEqual(
            invocations.last?.arguments,
            ["daemon", "restart", "--access", "lan", "--host", "192.168.1.20", "--listen", "0.0.0.0:7878",
             "--token-file", tokenPath, "--desktop-integration"]
        )
        _ = try await client.disableLAN()
        invocations = await launcher.invocations()
        XCTAssertEqual(
            invocations.last?.arguments,
            ["daemon", "restart", "--access", "local", "--listen", "127.0.0.1:7878",
             "--token-file", tokenPath, "--desktop-integration"]
        )
        _ = try await client.enableLAN()
        invocations = await launcher.invocations()
        XCTAssertEqual(
            invocations.last?.arguments,
            ["daemon", "restart", "--access", "lan", "--host", "192.168.1.20", "--listen", "0.0.0.0:7878",
             "--token-file", tokenPath, "--desktop-integration"]
        )
        XCTAssertEqual(try String(contentsOfFile: tokenPath, encoding: .utf8), token)
    }

    func testLANRejectsInsecureTokenFileBeforeRestart() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let tokenPath = home.appendingPathComponent("control.token").path
        XCTAssertTrue(FileManager.default.createFile(
            atPath: tokenPath, contents: Data("secret".utf8),
            attributes: [.posixPermissions: 0o644]
        ))
        let statusJSON = """
        {"ok":true,"state":"running","healthy":true,"home_dir":"\(home.path)",
        "socket_path":"/tmp/holon.sock","http_addr":"127.0.0.1:7878",
        "desired_running":true,"control_connectivity":true,"message":"Running"}
        """
        let launcher = RecordingProcessLauncher(
            result: .success(HolonProcessResult(
                terminationStatus: 0, stdout: Data(statusJSON.utf8), stderr: Data()
            )),
            address: "192.168.1.20\n"
        )
        let client = HolonCLIClient(executableURL: URL(fileURLWithPath: "/opt/holon"), launcher: launcher)
        do {
            _ = try await client.enableLAN()
            XCTFail("Expected insecure token file to be rejected")
        } catch let error as HolonCLIError {
            XCTAssertTrue(error.localizedDescription.contains(
                L10n.format("The control token file must be a nonempty, owner-only regular file: %@", tokenPath)
            ))
        }
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.arguments.starts(with: ["daemon", "restart"]) })
    }

    func testLANWithExistingAuthenticationDoesNotReplaceUnknownCredentials() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let (client, launcher) = serveClient(
            home: home.path, token: nil, lanAddress: "192.168.1.20",
            desktopIntegration: false
        )
        _ = try await client.enableLAN()
        let invocations = await launcher.invocations()
        XCTAssertEqual(invocations.last?.arguments, [
            "daemon", "restart", "--access", "lan", "--host", "192.168.1.20",
            "--listen", "0.0.0.0:7878", "--desktop-integration=false",
        ])
        XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent("control.token").path))
    }

    func testRestoringWildcardListenerDoesNotCreateOrReplaceCredentials() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let (client, launcher) = serveClient(
            address: "0.0.0.0:9000", home: home.path, token: nil,
            lanAddress: "192.168.1.20", desktopIntegration: false
        )
        _ = try await client.restart()
        let invocations = await launcher.invocations()
        XCTAssertEqual(invocations.last?.arguments, [
            "daemon", "restart", "--access", "lan", "--host", "192.168.1.20",
            "--listen", "0.0.0.0:9000", "--desktop-integration=false",
        ])
        XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent("control.token").path))
    }

    func testLocalStartupDoesNotLoadUnapprovedExistingTokenFile() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let path = home.appendingPathComponent("control.token").path
        XCTAssertTrue(FileManager.default.createFile(
            atPath: path, contents: Data("unapproved".utf8), attributes: [.posixPermissions: 0o600]
        ))
        let (client, launcher) = serveClient(home: home.path, token: nil)
        _ = try await client.start()
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.last?.arguments.contains("--token-file") == true)
        XCTAssertEqual(try String(contentsOfFile: path, encoding: .utf8), "unapproved")
    }

    func testStartRecordsOwnershipOnlyForExplicitlyCreatedProcess() async throws {
        for created in [true, false, nil] as [Bool?] {
            let suite = UUID().uuidString
            let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
            defer { preferences.removePersistentDomain(forName: suite) }
            let home = "/tmp/holon-\(UUID().uuidString)"
            let (client, _) = serveClient(home: home, processCreated: created, preferences: preferences)
            let status = try await client.start()
            XCTAssertEqual(status.processCreated, created)
            XCTAssertEqual(
                preferences.string(forKey: "HolonMenu.managedDaemonPID.\(home)"),
                created == true ? "99" : nil
            )
        }
    }

    func testServeRejectsPersistedOwnershipReusedAndUnknownAuthentication() async throws {
        for (created, authenticated) in [(false, false), (nil, false), (true, nil)] as [(Bool?, Bool?)] {
            let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            let suite = UUID().uuidString
            let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
            defer { preferences.removePersistentDomain(forName: suite) }
            preferences.set("99", forKey: "HolonMenu.managedDaemonPID.\(home.path)")
            ServeURLProtocol.lock.withLock {
                ServeURLProtocol.requests = []
                ServeURLProtocol.authenticationAvailable = authenticated
            }
            defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authenticationAvailable = true } }
            let (client, launcher) = serveClient(
                home: home.path, token: nil, processCreated: created, preferences: preferences
            )
            _ = try await client.start()
            do {
                _ = try await client.enableTailscaleServe()
                XCTFail("Uncertain provenance or authentication must fail closed")
            } catch {}
            let calls = await launcher.invocations()
            XCTAssertFalse(calls.contains { $0.arguments.contains("restart") })
            XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent("control.token").path))
            XCTAssertFalse(ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }.contains { $0.0 == "POST" })
        }
    }

    func testServePreparesAuthenticationOnlyForSessionCreatedDaemon() async throws {
        for address in ["127.0.0.1:7878", "0.0.0.0:9000", "[::]:9001"] {
            let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
            defer { try? FileManager.default.removeItem(at: home) }
            let suite = UUID().uuidString
            let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
            defer { preferences.removePersistentDomain(forName: suite) }
            ServeURLProtocol.lock.withLock {
                ServeURLProtocol.requests = []
                ServeURLProtocol.authenticationAvailable = false
                ServeURLProtocol.authenticateBearer = true
            }
            defer {
                ServeURLProtocol.lock.withLock {
                    ServeURLProtocol.authenticationAvailable = true
                    ServeURLProtocol.authenticateBearer = false
                }
            }
            let (client, launcher) = serveClient(
                address: address, home: home.path, token: nil,
                lanAddress: "192.168.1.10", processCreated: true, preferences: preferences
            )
            _ = try await client.start()
            _ = try await client.enableTailscaleServe()
            let calls = await launcher.invocations()
            XCTAssertEqual(calls.first { $0.arguments.contains("restart") }?.arguments, [
                "daemon", "restart",
                "--token-file", home.appendingPathComponent("control.token").path,
            ])
            let requests = ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }
            XCTAssertEqual(requests.map { $0.0 }, ["GET", "GET", "POST"])
            XCTAssertNil(requests[0].2)
            XCTAssertNotNil(requests[1].2)
            XCTAssertNotNil(requests[2].2)
            let attributes = try FileManager.default.attributesOfItem(
                atPath: home.appendingPathComponent("control.token").path
            )
            XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        }
    }

    func testServeRejectsExpiredSessionProcessProvenance() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.authenticationAvailable = false
        }
        defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authenticationAvailable = true } }
        let (client, launcher) = serveClient(home: home.path, token: nil, processCreated: true)
        _ = try await client.start()
        await launcher.replaceResult(HolonProcessResult(
            terminationStatus: 0,
            stdout: Data("""
                {"ok":true,"state":"running","healthy":true,"home_dir":"\(home.path)",
                "socket_path":"/tmp/holon.sock","http_addr":"127.0.0.1:7878",
                "desired_running":true,"control_connectivity":true,
                "executable_path":"/opt/holon","pid":100,"message":"Replaced"}
                """.utf8),
            stderr: Data()
        ))
        do {
            _ = try await client.enableTailscaleServe()
            XCTFail("Expired process provenance must fail closed")
        } catch {}
        let calls = await launcher.invocations()
        XCTAssertFalse(calls.contains { $0.arguments.contains("restart") })
        XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent("control.token").path))
        XCTAssertFalse(ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }.contains { $0.0 == "POST" })
    }

    func testServeRejectsAuthenticationStillDisabledAfterManagedRestart() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.authenticationAvailable = false
        }
        defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authenticationAvailable = true } }
        let (client, _) = serveClient(home: home.path, token: nil, processCreated: true)
        _ = try await client.start()
        do {
            _ = try await client.enableTailscaleServe()
            XCTFail("Authentication must be verified before POST")
        } catch {}
        XCTAssertFalse(ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }.contains { $0.0 == "POST" })
    }

    func testServeDoesNotClaimExternalDaemonReturnedByMenuStart() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let suite = UUID().uuidString
        let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { preferences.removePersistentDomain(forName: suite) }
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.authenticationAvailable = false
        }
        defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authenticationAvailable = true } }
        let (client, launcher) = serveClient(home: home.path, token: nil, processCreated: false, preferences: preferences)
        // daemon start can return the already-running same-binary process.
        _ = try await client.start()
        XCTAssertNil(preferences.string(forKey: "HolonMenu.managedDaemonPID.\(home.path)"))
        do {
            _ = try await client.enableTailscaleServe()
            XCTFail("A start result must not grant ownership or authorize auth preparation")
        } catch {
            XCTAssertEqual(error.localizedDescription, L10n.text("This daemon is externally managed or its ownership is unknown, or existing credentials conflict with disabled control authentication; an explicit daemon configuration change is required."))
        }
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.arguments.contains("restart") })
        XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent("control.token").path))
        let requests = ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }
        XCTAssertFalse(requests.contains { $0.0 == "POST" })
    }

    func testServeDisabledControlAuthenticationDoesNotReplaceConfiguredCredential() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.authenticationAvailable = false
        }
        defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authenticationAvailable = true } }
        let (client, launcher) = serveClient(home: home.path, token: "existing-token", processCreated: true)
        _ = try await client.start()
        do {
            _ = try await client.enableTailscaleServe()
            XCTFail("Disabled control authentication must not be silently overridden")
        } catch {
            XCTAssertEqual(error.localizedDescription, L10n.text("This daemon is externally managed or its ownership is unknown, or existing credentials conflict with disabled control authentication; an explicit daemon configuration change is required."))
        }
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.arguments.contains("restart") })
        XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent("control.token").path))
    }

    func testServeMissingAuthenticationDoesNotRestartUnknownSameBinaryDaemon() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.authenticationAvailable = false
        }
        defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authenticationAvailable = true } }
        let (client, launcher) = serveClient(home: home.path, token: nil)
        do {
            _ = try await client.enableTailscaleServe()
            XCTFail("Unknown ownership must not be inferred from executable path")
        } catch {
            XCTAssertEqual(error.localizedDescription, L10n.text("This daemon is externally managed or its ownership is unknown, or existing credentials conflict with disabled control authentication; an explicit daemon configuration change is required."))
        }
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.arguments.contains("restart") })
        XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent("control.token").path))
    }

    func testServeMissingAuthenticationDoesNotRestartExternalDaemon() async throws {
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.authenticationAvailable = false
        }
        defer { ServeURLProtocol.lock.withLock { ServeURLProtocol.authenticationAvailable = true } }
        let (client, launcher) = serveClient(token: nil, daemonExecutable: "/external/holon")
        do {
            _ = try await client.enableTailscaleServe()
            XCTFail("External daemon must not be restarted")
        } catch {
            XCTAssertEqual(error.localizedDescription, L10n.text("This daemon is externally managed or its ownership is unknown, or existing credentials conflict with disabled control authentication; an explicit daemon configuration change is required."))
        }
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.arguments.contains("restart") })
        let requests = ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }
        XCTAssertFalse(requests.contains { $0.0 == "POST" })
    }

    func testDisableServeOnlyRemovesHolonRootRule() async throws {
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.responseCode = 200
        }
        let (client, launcher) = serveClient()
        let status = try await client.tailscaleStatus()
        XCTAssertTrue(status.desiredEnabled)
        XCTAssertFalse(status.serving)
        _ = try await client.enableTailscaleServe()
        _ = try await client.disableTailscaleServe()
        let requests = ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }
        XCTAssertEqual(requests.map { "\($0.0) \($0.1)" }, [
            "GET /api/control/network/tailscale/serve",
            "GET /api/control/network/tailscale/serve",
            "POST /api/control/network/tailscale/serve/enable",
            "POST /api/control/network/tailscale/serve/disable"
        ])
        XCTAssertTrue(requests.allSatisfy { $0.2 == "Bearer test-token" })
        let invocations = await launcher.invocations()
        XCTAssertTrue(invocations.allSatisfy { $0.arguments == ["daemon", "status"] })
    }

    func testServeControlUsesLoopbackWhenDaemonReportsStaleLANAddress() async throws {
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.responseCode = 200
        }
        let (client, _) = serveClient(address: "192.168.11.2:7878")
        _ = try await client.tailscaleStatus()
        _ = try await client.enableTailscaleServe()
        _ = try await client.disableTailscaleServe()

        let requests = ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }
        XCTAssertEqual(requests.map { $0.3?.absoluteString }, [
            "http://127.0.0.1:7878/api/control/network/tailscale/serve",
            "http://127.0.0.1:7878/api/control/network/tailscale/serve",
            "http://127.0.0.1:7878/api/control/network/tailscale/serve/enable",
            "http://127.0.0.1:7878/api/control/network/tailscale/serve/disable"
        ])
    }

    func testServeControlUsesIPv6LoopbackWhenDaemonOnlyListensOnIPv6() async throws {
        ServeURLProtocol.lock.withLock {
            ServeURLProtocol.requests = []
            ServeURLProtocol.responseCode = 200
        }
        let (client, _) = serveClient(address: "[::]:7878")
        _ = try await client.tailscaleStatus()
        _ = try await client.enableTailscaleServe()
        _ = try await client.disableTailscaleServe()

        let requests = ServeURLProtocol.lock.withLock { ServeURLProtocol.requests }
        XCTAssertEqual(requests.map { $0.3?.absoluteString }, [
            "http://[::1]:7878/api/control/network/tailscale/serve",
            "http://[::1]:7878/api/control/network/tailscale/serve",
            "http://[::1]:7878/api/control/network/tailscale/serve/enable",
            "http://[::1]:7878/api/control/network/tailscale/serve/disable"
        ])
    }

    func testDisableServeRefusesUnrelatedRule() async throws {
        ServeURLProtocol.lock.withLock { ServeURLProtocol.responseCode = 409 }
        defer {
            ServeURLProtocol.lock.withLock { ServeURLProtocol.responseCode = 200 }
        }
        let (client, launcher) = serveClient()
        do {
            _ = try await client.disableTailscaleServe()
            XCTFail("Disabling an unrelated Serve rule must fail")
        } catch HolonCLIError.tailscaleServeConflict {
            // The other service is not managed by Holon.
        }
        do {
            _ = try await client.enableTailscaleServe()
            XCTFail("Enabling Holon Serve must not replace an unrelated rule")
        } catch HolonCLIError.tailscaleServeConflict {
            // The existing root rule belongs to another service.
        }
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.arguments.contains("off") })
        XCTAssertFalse(invocations.contains { $0.arguments.contains("--bg") })
    }
}
