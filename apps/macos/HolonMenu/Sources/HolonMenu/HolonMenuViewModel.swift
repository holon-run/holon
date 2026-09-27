import AppKit
import Foundation
import SwiftUI

protocol MenuURLOpening {
    func open(_ url: URL)
}

struct SystemMenuURLOpener: MenuURLOpening {
    func open(_ url: URL) {
        NSWorkspace.shared.open(url)
    }
}

@MainActor
final class HolonMenuViewModel: ObservableObject {
    @Published private(set) var status: HolonDaemonStatus?
    @Published private(set) var isPolling = false
    @Published private(set) var activeOperation: String?
    @Published private(set) var tailscaleStatus: HolonTailscaleStatus?
    @Published private(set) var lanURL: URL?
    @Published var showTailscaleServeConfirmation = false
    @Published var showLANConfirmation = false
    @Published var launchAtLoginEnabled = false
    @Published var lastError: String?
    @Published var commandLineToolMessage: String?

    private let client: any HolonDesiredStateClient
    private let opener: MenuURLOpening
    private var pollingTask: Task<Void, Never>?

    init(client: some HolonDesiredStateClient, opener: some MenuURLOpening = SystemMenuURLOpener()) {
        self.client = client
        self.opener = opener
    }

    deinit {
        pollingTask?.cancel()
    }

    func bootstrap() async {
        await refresh()
        if status?.desiredRunning == true {
            switch status?.state {
            case .stopped:
                await start()
            case .versionMismatch:
                await restart()
            default:
                break
            }
        }
        startPolling()
    }

    func refresh() async {
        do {
            status = try await client.status()
            tailscaleStatus = try await client.tailscaleStatus()
            lanURL = try await client.lanURL()
            launchAtLoginEnabled = try await client.launchAtLoginEnabled()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func start() async {
        await runOperation { try await self.client.start() }
    }

    func stop() async {
        await runOperation { try await self.client.stop() }
    }

    func restart() async {
        await runOperation { try await self.client.restart() }
    }

    func requestTailscaleServe() {
        showTailscaleServeConfirmation = true
    }

    func requestLANAccess() {
        showLANConfirmation = true
    }

    func enableLAN() async {
        showLANConfirmation = false
        activeOperation = "Enabling LAN access…"
        defer { activeOperation = nil }
        do {
            lanURL = try await client.enableLAN()
            status = try await client.status()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func disableLAN() async {
        activeOperation = "Disabling LAN access…"
        defer { activeOperation = nil }
        do {
            status = try await client.disableLAN()
            lanURL = nil
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func enableTailscaleServe() async {
        showTailscaleServeConfirmation = false
        activeOperation = "Updating Tailscale…"
        defer { activeOperation = nil }
        do {
            tailscaleStatus = try await client.enableTailscaleServe()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func disableTailscaleServe() async {
        activeOperation = "Updating Tailscale…"
        defer { activeOperation = nil }
        do {
            tailscaleStatus = try await client.disableTailscaleServe()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func openWeb() async {
        do {
            let url = try await client.webURL()
            opener.open(url)
        } catch {
            lastError = error.localizedDescription
        }
    }

    func openLogs() async {
        do {
            let url = try await client.logsURL()
            opener.open(url)
        } catch {
            lastError = error.localizedDescription
        }
    }

    func setLaunchAtLogin(_ enabled: Bool) async {
        do {
            try await client.setLaunchAtLoginEnabled(enabled)
            launchAtLoginEnabled = try await client.launchAtLoginEnabled()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func installCommandLineTool() async {
        do {
            let destination = try await client.installCommandLineTool()
            commandLineToolMessage =
                "Installed at \(destination.path). Add ~/.local/bin to PATH if needed."
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func startPolling(intervalNanoseconds: UInt64 = 3_000_000_000) {
        pollingTask?.cancel()
        isPolling = true
        pollingTask = Task { [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                await self.refresh()
                try? await Task.sleep(nanoseconds: intervalNanoseconds)
            }
        }
    }

    func stopPolling() {
        pollingTask?.cancel()
        pollingTask = nil
        isPolling = false
    }

    var stateTitle: String {
        status?.state.title ?? "Unknown"
    }

    var statusMessage: String {
        status?.message ?? "Waiting for Holon status."
    }

    var webAddressText: String {
        lanURL?.absoluteString ?? status?.webUrl ?? status?.httpAddr ?? "No web endpoint yet."
    }

    var connectionURL: URL? {
        tailscaleStatus?.serveURL ?? lanURL
    }

    var isRunning: Bool {
        status?.state == .running || status?.state == .degraded
    }

    var isOperating: Bool {
        activeOperation != nil
    }

    private func runOperation(_ operation: @escaping () async throws -> HolonDaemonStatus) async {
        activeOperation = "Updating Holon…"
        defer { activeOperation = nil }
        do {
            let updated = try await operation()
            status = updated
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }
}
