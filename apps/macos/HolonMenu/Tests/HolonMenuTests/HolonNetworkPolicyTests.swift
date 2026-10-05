import Foundation
import XCTest
@testable import HolonMenu

final class HolonNetworkPolicyTests: XCTestCase {
    func testExplicitListenerNeverCombinesWithPortAndDesktopIsIndependent() {
        let options = HolonDaemonLaunchOptions(
            access: "lan", host: "192.168.1.20", listen: "0.0.0.0:9000",
            port: 7878, desktopIntegration: false
        )
        let arguments = options.arguments()
        XCTAssertFalse(arguments.contains("--port"))
        XCTAssertTrue(arguments.contains("--desktop-integration=false"))
        XCTAssertEqual(HolonDaemonLaunchOptions(access: "lan").arguments().last, "--desktop-integration")
        XCTAssertFalse(HolonDaemonLaunchOptions(desktopIntegration: nil).arguments()
            .contains(where: { $0.hasPrefix("--desktop-integration") }))
        XCTAssertTrue(HolonDaemonLaunchOptions.default.arguments().contains("--desktop-integration"))
    }

    @MainActor
    func testCustomOriginRejectsLocalAliasesAndMappedLoopback() {
        let model = HolonMenuViewModel(client: FakeHolonClient())
        for origin in [
            "http://sub.localhost", "http://sub.localhost.", "http://127.1",
            "http://[::ffff:127.0.0.1]", "http://[::ffff:7f00:1]",
            "http://[::ffff:0.0.0.0]",
        ] {
            model.customPairingOrigin = origin
            XCTAssertNil(model.connectionURL, origin)
        }
        model.customPairingOrigin = "https://holon.example.com"
        XCTAssertNotNil(model.connectionURL)
    }

    func testServeOriginRequiresKnownNonConflictingMatchingHTTPSOrigin() {
        var status = HolonTailscaleStatus(
            state: .serving, hostname: "holon.example.ts.net",
            serveURL: URL(string: "https://holon.example.ts.net"), message: "",
            serving: true, statusKnown: true
        )
        XCTAssertNotNil(status.pairingOrigin)
        for invalid in [
            "http://holon.example.ts.net", "https://other.example.ts.net",
            "https://user@holon.example.ts.net", "https://holon.example.ts.net/path",
            "https://holon.example.ts.net?query=1", "https://holon.example.ts.net#fragment",
            "https://holon.example.ts.net:8443",
        ] {
            status.serveURL = URL(string: invalid)
            XCTAssertNil(status.pairingOrigin, invalid)
        }
        status.serveURL = URL(string: "https://holon.example.ts.net")
        status.conflict = true
        XCTAssertNil(status.pairingOrigin)
        status.conflict = false
        status.statusKnown = false
        XCTAssertNil(status.pairingOrigin)
        status.statusKnown = true
        status.serving = false
        XCTAssertNil(status.pairingOrigin)
        status.serving = true
        status.state = .stopped
        XCTAssertNil(status.pairingOrigin)
    }
}

@MainActor
final class HolonPairingPolicyTests: XCTestCase {
    func testTicketExpiresWithoutAutomaticallyIssuingAnother() async throws {
        let client = FakeHolonClient()
        let model = HolonMenuViewModel(client: client, pairingLifetime: .milliseconds(1))
        await model.enableLAN()
        await model.showPairingCode()
        XCTAssertNotNil(model.pairingURL)
        try await Task.sleep(for: .milliseconds(25))
        XCTAssertNil(model.pairingURL)
        let commands = await client.recordedCommands()
        XCTAssertEqual(commands.filter {
            if case .pairingURL = $0 { return true }
            return false
        }.count, 1)
    }

    func testUnknownNetworkStatusClearsExistingServeTicketAndFallsBackToLAN() async {
        let client = FakeHolonClient()
        let model = HolonMenuViewModel(client: client)
        await model.enableLAN()
        await model.enableTailscaleServe()
        await model.showPairingCode()
        XCTAssertNotNil(model.pairingURL)
        await client.setTailscaleStatusError(HolonCLIError.tailscaleServeConflict("unknown"))
        await model.refresh()
        XCTAssertNil(model.pairingURL)
        XCTAssertEqual(model.connectionURL?.absoluteString, "http://192.168.1.20:7878")
    }

    func testRefreshIsolatesServeFailureFromLANAndDaemonStatus() async {
        let client = FakeHolonClient(tailscaleStatusError: HolonCLIError.tailscaleServeConflict("unavailable"))
        let model = HolonMenuViewModel(client: client)
        await model.enableLAN()
        await model.refresh()
        XCTAssertNotNil(model.status)
        XCTAssertNotNil(model.lanURL)
        XCTAssertNil(model.tailscaleStatus)
        XCTAssertEqual(model.tailscaleError, "unavailable")
        XCTAssertNil(model.lastError)
    }

    func testRefreshIsolatesLANFailureFromServeStatus() async {
        let client = FakeHolonClient(lanStatusError: HolonCLIError.invalidWebAddress("missing"))
        let model = HolonMenuViewModel(client: client)
        await model.enableTailscaleServe()
        await model.refresh()
        XCTAssertNotNil(model.status)
        XCTAssertNil(model.lanURL)
        XCTAssertNotNil(model.tailscaleStatus?.pairingOrigin)
        XCTAssertNotNil(model.lanError)
        XCTAssertNil(model.lastError)
    }

    func testCustomServeLANPriorityAndChangesClearTicketsWithoutIssuing() async {
        let client = FakeHolonClient()
        let model = HolonMenuViewModel(client: client)
        await model.enableLAN()
        await model.enableTailscaleServe()
        XCTAssertEqual(model.connectionURL?.absoluteString, "https://holon.example.ts.net")
        await model.showPairingCode()
        XCTAssertNotNil(model.pairingURL)
        model.preferLANPairing = true
        XCTAssertNil(model.pairingURL)
        XCTAssertEqual(model.connectionURL?.absoluteString, "http://192.168.1.20:7878")
        model.customPairingOrigin = "https://custom.example"
        XCTAssertEqual(model.connectionURL?.absoluteString, "https://custom.example")
        model.customPairingOrigin = "https://custom.example/path"
        XCTAssertNil(model.connectionURL)
        let commands = await client.recordedCommands()
        XCTAssertEqual(commands.filter {
            if case .pairingURL = $0 { return true }
            return false
        }.count, 1)
    }

    func testNetworkOperationClearsExistingTicketEvenIfOperationFails() async {
        let client = FakeHolonClient(
            enableTailscaleServeError: HolonCLIError.tailscaleServeConflict("conflict")
        )
        let model = HolonMenuViewModel(client: client)
        await model.enableLAN()
        await model.showPairingCode()
        XCTAssertNotNil(model.pairingURL)
        await model.enableTailscaleServe()
        XCTAssertNil(model.pairingURL)
        XCTAssertEqual(model.tailscaleError, "conflict")
    }
}
