import XCTest
@testable import HolonMenu

@MainActor
final class HolonMenuViewModelTests: XCTestCase {
    func testBootstrapReplacesIncompatibleDesiredRuntime() async {
        let client = FakeHolonClient(
            currentStatus: HolonDaemonStatus(
                ok: true,
                state: .versionMismatch,
                healthy: false,
                homeDir: "/Users/holon/.holon",
                socketPath: "/tmp/holon.sock",
                httpAddr: "127.0.0.1:7878",
                webUrl: "http://127.0.0.1:7878",
                productVersion: nil,
                controlProtocolVersion: 0,
                lifecycleOwner: "standalone",
                executablePath: "/usr/local/bin/holon",
                desiredRunning: true,
                pid: 42,
                controlConnectivity: false,
                runtimeConfigFingerprint: nil,
                configFingerprintMatch: nil,
                message: "Runtime version mismatch."
            )
        )
        let viewModel = HolonMenuViewModel(client: client)

        await viewModel.bootstrap()
        viewModel.stopPolling()

        let commands = await client.recordedCommands()
        XCTAssertEqual(
            Array(commands.prefix(3)),
            [.status, .launchAtLoginEnabled, .restart]
        )
        XCTAssertEqual(viewModel.status?.state, .running)
    }
}
