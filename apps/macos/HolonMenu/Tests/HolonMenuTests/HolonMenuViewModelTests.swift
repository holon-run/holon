import XCTest
@testable import HolonMenu

@MainActor
final class HolonMenuViewModelTests: XCTestCase {
    func testOpenWebRequestsAnAuthenticatedLocalURL() async {
        let client = FakeHolonClient()
        let viewModel = HolonMenuViewModel(client: client)
        await viewModel.openWeb()
        let commands = await client.recordedCommands()
        XCTAssertEqual(commands, [.authenticatedWebURL])
    }

    func testPairingQRIsRequestedOnlyOnDemandAndCanBeHidden() async {
        let client = FakeHolonClient()
        let viewModel = HolonMenuViewModel(client: client)
        await viewModel.refresh()
        XCTAssertNil(viewModel.pairingURL)
        await viewModel.enableLAN()
        await viewModel.showPairingCode()
        XCTAssertTrue(viewModel.pairingURL?.absoluteString.contains("#pair=") == true)
        let commands = await client.recordedCommands()
        XCTAssertEqual(commands.last,
                       .pairingURL(URL(string: "http://192.168.1.20:7878")!))
        viewModel.hidePairingCode()
        XCTAssertNil(viewModel.pairingURL)
    }

    func testLANAddressIsDisplayedInsteadOfLoopback() async {
        let client = FakeHolonClient()
        let viewModel = HolonMenuViewModel(client: client)
        await viewModel.enableLAN()
        XCTAssertEqual(viewModel.webAddressText, "http://192.168.1.20:7878")
        viewModel.stopPolling()
    }

    func testLANFailureRemainsVisibleAfterPollingRefresh() async {
        let client = FakeHolonClient(
            enableLANError: NSError(
                domain: "LAN test",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "LAN restart failed"]
            )
        )
        let viewModel = HolonMenuViewModel(client: client)

        viewModel.requestLANAccess()
        XCTAssertTrue(viewModel.showLANConfirmation)
        await viewModel.enableLAN()
        await viewModel.refresh()

        XCTAssertFalse(viewModel.showLANConfirmation)
        XCTAssertEqual(viewModel.lanError, "LAN restart failed")
        XCTAssertNil(viewModel.lanURL)
        viewModel.stopPolling()
    }

    func testTailscaleFailureRemainsVisibleAfterPollingRefresh() async {
        let client = FakeHolonClient(
            enableTailscaleServeError: HolonCLIError.tailscaleServeConflict(
                "Tailscale Serve already exposes another service at /; leave its configuration unchanged."
            )
        )
        let viewModel = HolonMenuViewModel(client: client)

        viewModel.requestTailscaleServe()
        XCTAssertTrue(viewModel.showTailscaleServeConfirmation)
        await viewModel.enableTailscaleServe()
        await viewModel.refresh()

        XCTAssertFalse(viewModel.showTailscaleServeConfirmation)
        XCTAssertEqual(
            viewModel.tailscaleError,
            "Tailscale Serve already exposes another service at /; leave its configuration unchanged."
        )
        XCTAssertEqual(viewModel.tailscaleStatus?.state, .connected)
        viewModel.stopPolling()
    }

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
            Array(commands.prefix(4)),
            [.status, .tailscaleStatus, .lanURL, .launchAtLoginEnabled]
        )
        XCTAssertEqual(commands.last, .restart)
        XCTAssertEqual(viewModel.status?.state, .running)
    }
}
