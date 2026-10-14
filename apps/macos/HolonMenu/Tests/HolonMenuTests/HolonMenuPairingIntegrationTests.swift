import Foundation
import XCTest
@testable import HolonMenu

private actor UnixPairingLauncher: HolonProcessLaunching {
    private let status: Data
    private var calls: [[String]] = []

    init(home: String, address: String, socket: String) throws {
        status = try JSONSerialization.data(withJSONObject: [
            "ok": true, "state": "running", "healthy": true, "home_dir": home,
            "socket_path": socket, "http_addr": address, "web_url": "http://\(address)",
            "desired_running": true, "control_connectivity": true, "message": "Isolated test",
        ])
    }

    func run(executableURL: URL, arguments: [String]) async throws -> HolonProcessResult {
        calls.append(arguments)
        if arguments == ["daemon", "status"] {
            return HolonProcessResult(terminationStatus: 0, stdout: status, stderr: Data())
        }
        return try await SystemHolonProcessLauncher().run(
            executableURL: executableURL, arguments: arguments
        )
    }

    func invocations() -> [[String]] { calls }
}

final class HolonMenuPairingIntegrationTests: XCTestCase {
    // Opt-in fixture: a separate daemon with its own fresh home, loopback port,
    // Unix socket, and only the synthetic token below; never use the live daemon.
    func testPairingAgainstIsolatedDaemon() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let home = environment["HOLON_MENU_PAIRING_TEST_HOME"],
              let address = environment["HOLON_MENU_PAIRING_TEST_ADDRESS"],
              let socket = environment["HOLON_MENU_PAIRING_TEST_SOCKET"],
              let expectedStatus = environment["HOLON_MENU_PAIRING_TEST_TCP_STATUS"] else {
            throw XCTSkip("Requires an explicitly isolated pairing test daemon")
        }
        let base = try XCTUnwrap(URL(string: "http://\(address)"))
        guard base.host == "127.0.0.1", base.port != nil, base.port != 7878,
              socket.hasPrefix(home + "/"), ["200", "401"].contains(expectedStatus) else {
            XCTFail("Fixture must use an isolated home/socket and a non-production loopback port")
            return
        }
        let session = URLSession(configuration: .ephemeral)
        defer { session.invalidateAndCancel() }
        var issue = URLRequest(url: base.appendingPathComponent("api/auth/pairing/issue"))
        issue.httpMethod = "POST"
        issue.setValue("Bearer isolated-menu-test-token", forHTTPHeaderField: "Authorization")
        let (_, issueResponse) = try await session.data(for: issue)
        XCTAssertEqual((issueResponse as? HTTPURLResponse)?.statusCode, Int(expectedStatus))

        let suite = "holon-pairing-integration-\(UUID().uuidString)"
        let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { preferences.removePersistentDomain(forName: suite) }
        let launcher = try UnixPairingLauncher(home: home, address: address, socket: socket)
        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/isolated-holon"),
            launcher: launcher, networkSession: session, preferences: preferences
        )
        let url = try await client.pairingURL(for: URL(string: "https://isolated.example.ts.net")!)
        XCTAssertEqual(url.scheme, "https")
        XCTAssertEqual(url.host, "isolated.example.ts.net")
        XCTAssertEqual(url.path, "/login")
        let fragment = try XCTUnwrap(url.fragment)
        XCTAssertTrue(fragment.hasPrefix("pair="))
        let ticket = String(fragment.dropFirst("pair=".count))
        XCTAssertFalse(ticket.isEmpty)
        let calls = await launcher.invocations()
        XCTAssertEqual(calls.count, 2)
        XCTAssertTrue(calls.last?.contains("--unix-socket") == true)
        XCTAssertFalse(calls.last?.contains { $0.contains("isolated-menu-test-token") } == true)

        // A Unix-issued code must redeem exactly once on the network listener.
        var redeem = URLRequest(url: base.appendingPathComponent("api/auth/pairing/redeem"))
        redeem.httpMethod = "POST"
        redeem.setValue("application/json", forHTTPHeaderField: "Content-Type")
        redeem.setValue(base.absoluteString, forHTTPHeaderField: "Origin")
        redeem.httpBody = try JSONSerialization.data(withJSONObject: ["ticket": ticket])
        for expected in [200, 401] {
            let (_, response) = try await session.data(for: redeem)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, expected)
        }
    }
}
