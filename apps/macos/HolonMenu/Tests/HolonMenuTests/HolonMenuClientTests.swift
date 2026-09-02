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

    init(result: Result<HolonProcessResult, Error>) {
        self.result = result
    }

    func run(executableURL: URL, arguments: [String]) async throws -> HolonProcessResult {
        recordedInvocations.append(Invocation(executableURL: executableURL, arguments: arguments))
        return try result.get()
    }

    func invocations() -> [Invocation] {
        recordedInvocations
    }
}

final class HolonMenuClientTests: XCTestCase {
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
          "product_version": "0.35.0 (abcdef0)",
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
            ["daemon", "start", "--access", "local", "--port", "7878"]
        )
    }
}
