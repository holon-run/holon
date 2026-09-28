import Foundation
import XCTest
@testable import HolonMenu

actor RecordingProcessLauncher: HolonProcessLaunching {
    struct Invocation: Equatable, Sendable {
        let executableURL: URL
        let arguments: [String]
    }

    private var recordedInvocations: [Invocation] = []
    private let result: Result<HolonProcessResult, Error>
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
}

final class HolonMenuClientTests: XCTestCase {
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
          "product_version": "0.46.0 (abcdef0)",
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

    func testLANUsesClientVisibleAddressWithoutDesktopIntegration() async throws {
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
            launchOptions: HolonDaemonLaunchOptions(access: "local", port: 7878)
        )
        let webURL = try await client.webURL()
        XCTAssertEqual(webURL.absoluteString, "http://192.168.1.20:7878")
        let addressLookup = await launcher.invocations().first {
            $0.arguments.first == "getifaddr"
        }
        XCTAssertEqual(addressLookup?.executableURL.path, "/usr/sbin/ipconfig")
        XCTAssertEqual(addressLookup?.arguments, ["getifaddr", "en0"])
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
            ["daemon", "restart", "--access", "lan", "--host", "192.168.1.20", "--port", "7878",
             "--token-file", tokenPath, "--desktop-integration=false"]
        )
        _ = try await client.disableLAN()
        invocations = await launcher.invocations()
        XCTAssertEqual(
            invocations.last?.arguments,
            ["daemon", "restart", "--access", "local", "--listen", "127.0.0.1:7878", "--desktop-integration"]
        )
        _ = try await client.enableLAN()
        invocations = await launcher.invocations()
        XCTAssertEqual(
            invocations.last?.arguments,
            ["daemon", "restart", "--access", "lan", "--host", "192.168.1.20", "--port", "7878",
             "--token-file", tokenPath, "--desktop-integration=false"]
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
            XCTAssertEqual(error.localizedDescription,
                           "The LAN token file must be a nonempty, owner-only regular file: \(tokenPath)")
        }
        let invocations = await launcher.invocations()
        XCTAssertFalse(invocations.contains { $0.arguments.starts(with: ["daemon", "restart"]) })
    }

    func testDisableServeOnlyRemovesHolonRootRule() async throws {
        let status = #"{"BackendState":"Running","Self":{"DNSName":"holon.example.ts.net."}}"#
        let serve = #"{"Web":{"holon.example.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:7878"},"/other":{"Proxy":"http://127.0.0.1:9000"}}}}}"#
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
            responses: ["status --json": status, "serve status --json": serve]
        )
        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher,
            tailscaleExecutableURL: URL(fileURLWithPath: "/opt/tailscale")
        )
        _ = try await client.disableTailscaleServe()
        let invocations = await launcher.invocations()
        XCTAssertTrue(invocations.contains {
            $0.arguments == ["serve", "--https=443", "--set-path=/", "off"]
        })
        XCTAssertFalse(invocations.contains { $0.arguments == ["serve", "reset"] })
    }

    func testDisableServeRefusesUnrelatedRule() async throws {
        let status = #"{"BackendState":"Running","Self":{"DNSName":"holon.example.ts.net."}}"#
        let serve = #"{"Web":{"holon.example.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:9000"}}}}}"#
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
            responses: ["status --json": status, "serve status --json": serve]
        )
        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher,
            tailscaleExecutableURL: URL(fileURLWithPath: "/opt/tailscale")
        )
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
