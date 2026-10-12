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
        await viewModel.refresh()

        viewModel.requestTailscaleServe()
        XCTAssertTrue(viewModel.showTailscaleServeConfirmation)
        await viewModel.enableTailscaleServe()
        await viewModel.refresh()

        XCTAssertFalse(viewModel.showTailscaleServeConfirmation)
        XCTAssertEqual(
            viewModel.tailscaleError,
            L10n.text("Tailscale Serve already exposes another service at /; leave its configuration unchanged.")
        )
        XCTAssertEqual(viewModel.tailscaleStatus?.state, .connected)
        viewModel.stopPolling()
    }

    func testTailscaleProbeErrorClearsAfterSuccessfulRefresh() async {
        let client = FakeHolonClient(
            tailscaleStatusError: HolonCLIError.tailscaleServeConflict("Status probe failed")
        )
        let viewModel = HolonMenuViewModel(client: client)
        await viewModel.refresh()
        XCTAssertEqual(viewModel.tailscaleError, "Status probe failed")
        XCTAssertNil(viewModel.tailscaleStatus)

        await client.setTailscaleStatusError(nil)
        await viewModel.refresh()
        XCTAssertNil(viewModel.tailscaleProbeError)
        XCTAssertNil(viewModel.tailscaleError)
        XCTAssertEqual(viewModel.tailscaleStatus?.state, .connected)
    }

    func testTailscaleUnavailableDoesNotReportProbeError() async {
        let client = FakeHolonClient(tailscaleStatus: HolonTailscaleStatus(
            state: .unavailable, hostname: nil, serveURL: nil,
            message: "Tailscale CLI not found (optional); install it or configure its path"
        ))
        let viewModel = HolonMenuViewModel(client: client)
        await viewModel.refresh()

        XCTAssertNil(viewModel.tailscaleError)
        XCTAssertEqual(viewModel.tailscaleStatus?.title, L10n.text("CLI not found (optional)"))
    }

    func testTailscaleServeRequiresConnectedKnownNonconflictingState() async {
        let client = FakeHolonClient()
        let viewModel = HolonMenuViewModel(client: client)
        viewModel.requestTailscaleServe()
        XCTAssertFalse(viewModel.showTailscaleServeConfirmation)
        await viewModel.enableTailscaleServe()

        for state in [HolonTailscaleState.unavailable, .stopped, .loggedOut, .unknown, .serving] {
            await client.setTailscaleStatus(HolonTailscaleStatus(
                state: state, hostname: nil, serveURL: nil, message: state.rawValue,
                statusKnown: true, available: true, connected: true
            ))
            await viewModel.refresh()
            viewModel.requestTailscaleServe()
            XCTAssertFalse(viewModel.showTailscaleServeConfirmation, state.rawValue)
            await viewModel.enableTailscaleServe()
        }
        for (available, connected, known, conflict) in [
            (false, true, true, false), (true, false, true, false),
            (true, true, false, false), (true, true, true, true),
        ] {
            await client.setTailscaleStatus(HolonTailscaleStatus(
                state: .connected, hostname: nil, serveURL: nil, message: "Connected",
                conflict: conflict, statusKnown: known, available: available, connected: connected
            ))
            await viewModel.refresh()
            viewModel.requestTailscaleServe()
            XCTAssertFalse(viewModel.showTailscaleServeConfirmation)
            await viewModel.enableTailscaleServe()
        }
        let commands = await client.recordedCommands()
        XCTAssertFalse(commands.contains(.enableTailscaleServe))
        XCTAssertNil(viewModel.tailscaleError)
    }

    func testTailscaleRefreshRevokesConfirmationWhenNoLongerExecutable() async {
        let client = FakeHolonClient()
        let viewModel = HolonMenuViewModel(client: client)
        for state in [HolonTailscaleState.unavailable, .stopped, .loggedOut, .unknown, .serving] {
            await client.setTailscaleStatus(HolonTailscaleStatus(
                state: .connected, hostname: nil, serveURL: nil, message: "Connected",
                statusKnown: true, available: true, connected: true
            ))
            await viewModel.refresh()
            viewModel.requestTailscaleServe()
            XCTAssertTrue(viewModel.showTailscaleServeConfirmation)
            await client.setTailscaleStatus(HolonTailscaleStatus(
                state: state, hostname: nil, serveURL: nil, message: state.rawValue,
                statusKnown: true, available: true, connected: true
            ))
            await viewModel.refresh()
            XCTAssertFalse(viewModel.showTailscaleServeConfirmation, state.rawValue)
        }
    }

    func testTailscaleRefreshRevokesConfirmationWhenBackendBooleanGateChanges() async {
        let client = FakeHolonClient()
        let viewModel = HolonMenuViewModel(client: client)
        for (available, connected) in [(false, true), (true, false)] {
            await client.setTailscaleStatus(HolonTailscaleStatus(
                state: .connected, hostname: nil, serveURL: nil, message: "Connected",
                statusKnown: true, available: true, connected: true
            ))
            await viewModel.refresh()
            viewModel.requestTailscaleServe()
            XCTAssertTrue(viewModel.showTailscaleServeConfirmation)
            await client.setTailscaleStatus(HolonTailscaleStatus(
                state: .connected, hostname: nil, serveURL: nil, message: "Connected",
                statusKnown: true, available: available, connected: connected
            ))
            await viewModel.refresh()
            XCTAssertFalse(viewModel.showTailscaleServeConfirmation)
        }
    }

    func testTailscaleProbeFailureRevokesConfirmation() async {
        let client = FakeHolonClient()
        let viewModel = HolonMenuViewModel(client: client)
        await viewModel.refresh()
        viewModel.requestTailscaleServe()
        XCTAssertTrue(viewModel.showTailscaleServeConfirmation)

        await client.setTailscaleStatusError(HolonCLIError.tailscaleServeConflict("Probe failed"))
        await viewModel.refresh()
        XCTAssertFalse(viewModel.showTailscaleServeConfirmation)
        await viewModel.enableTailscaleServe()
        let commands = await client.recordedCommands()
        XCTAssertFalse(commands.contains(.enableTailscaleServe))
    }

    func testTailscaleOperationErrorHasPriorityAndClearsOnSuccessfulRetry() async {
        let client = FakeHolonClient(
            enableTailscaleServeError: HolonCLIError.tailscaleServeConflict("Enable failed")
        )
        let viewModel = HolonMenuViewModel(client: client)
        await viewModel.refresh()
        await viewModel.enableTailscaleServe()
        await client.setTailscaleStatusError(HolonCLIError.tailscaleServeConflict("Probe failed"))
        await viewModel.refresh()
        XCTAssertEqual(viewModel.tailscaleProbeError, "Probe failed")
        XCTAssertEqual(viewModel.tailscaleError, "Enable failed")

        await client.setTailscaleStatusError(nil)
        await viewModel.refresh()
        XCTAssertNil(viewModel.tailscaleProbeError)
        XCTAssertEqual(viewModel.tailscaleError, "Enable failed")

        await client.setEnableTailscaleServeError(nil)
        await viewModel.enableTailscaleServe()
        XCTAssertNil(viewModel.tailscaleOperationError)
        XCTAssertNil(viewModel.tailscaleError)
        XCTAssertEqual(viewModel.tailscaleStatus?.state, .serving)
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
