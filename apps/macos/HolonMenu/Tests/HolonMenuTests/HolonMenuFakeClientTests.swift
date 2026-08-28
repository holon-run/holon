import XCTest
@testable import HolonMenu

final class HolonMenuFakeClientTests: XCTestCase {
    func testFakeClientTracksStateTransitionsAndLoginItemState() async throws {
        let client = FakeHolonClient()

        let initial = try await client.status()
        XCTAssertEqual(initial.state, .stopped)
        XCTAssertEqual(initial.state.title, "Stopped")

        let running = try await client.start()
        XCTAssertEqual(running.state, .running)

        let webURL = try await client.webURL()
        XCTAssertEqual(webURL.absoluteString, "http://127.0.0.1:7878")

        let logsURL = try await client.logsURL()
        XCTAssertTrue(logsURL.path.hasSuffix("/run/daemon.log"))

        try await client.setLaunchAtLoginEnabled(true)
        let launchAtLoginEnabled = try await client.launchAtLoginEnabled()
        XCTAssertTrue(launchAtLoginEnabled)

        let commands = await client.recordedCommands()
        XCTAssertEqual(
            commands,
            [
                .status,
                .start,
                .webURL,
                .logsURL,
                .setLaunchAtLoginEnabled(true),
                .launchAtLoginEnabled,
            ]
        )
    }
}
