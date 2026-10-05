import AppKit
import Foundation
import SwiftUI
import Darwin

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
    @Published private(set) var tailscaleError: String?
    @Published private(set) var lanURL: URL?
    @Published private(set) var lanError: String?
    @Published private(set) var pairingURL: URL?
    @Published private(set) var pairingError: String?
    @Published var customPairingOrigin = "" {
        didSet { if oldValue != customPairingOrigin { hidePairingCode() } }
    }
    @Published var preferLANPairing = false {
        didSet { if oldValue != preferLANPairing { hidePairingCode() } }
    }
    @Published var showTailscaleServeConfirmation = false
    @Published var showLANConfirmation = false
    @Published var launchAtLoginEnabled = false
    @Published var lastError: String?
    @Published var commandLineToolMessage: String?

    private let client: any HolonDesiredStateClient
    private let opener: MenuURLOpening
    private let pairingLifetime: Duration
    private var pollingTask: Task<Void, Never>?
    private var pairingExpiryTask: Task<Void, Never>?
    private var pairingRevision: UInt64 = 0

    init(
        client: some HolonDesiredStateClient,
        opener: some MenuURLOpening = SystemMenuURLOpener(),
        pairingLifetime: Duration = .seconds(120)
    ) {
        self.client = client
        self.opener = opener
        self.pairingLifetime = pairingLifetime
    }

    deinit {
        pollingTask?.cancel()
        pairingExpiryTask?.cancel()
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
        let previousStatus = status
        let previousConnection = connectionURL
        let previousNetwork = tailscaleStatus
        let previousLAN = lanURL
        do {
            status = try await client.status()
            lastError = nil
        } catch {
            status = nil
            lastError = error.localizedDescription
        }
        do {
            tailscaleStatus = try await client.tailscaleStatus()
        } catch {
            tailscaleStatus = nil
            tailscaleError = error.localizedDescription
        }
        do {
            lanURL = try await client.lanURL()
        } catch {
            lanURL = nil
            lanError = error.localizedDescription
        }
        if previousStatus?.pid != status?.pid || previousStatus?.httpAddr != status?.httpAddr
            || previousStatus?.state != status?.state || previousConnection != connectionURL
            || previousNetwork != tailscaleStatus || previousLAN != lanURL {
            hidePairingCode()
        }
        do {
            launchAtLoginEnabled = try await client.launchAtLoginEnabled()
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
        hidePairingCode()
        showLANConfirmation = false
        lanError = nil
        activeOperation = "Enabling LAN access…"
        defer { activeOperation = nil }
        do {
            lanURL = try await client.enableLAN()
            status = try await client.status()
            lastError = nil
        } catch {
            lanError = error.localizedDescription
        }
    }

    func disableLAN() async {
        hidePairingCode()
        lanError = nil
        activeOperation = "Disabling LAN access…"
        defer { activeOperation = nil }
        do {
            status = try await client.disableLAN()
            lanURL = nil
            lastError = nil
        } catch {
            lanError = error.localizedDescription
        }
    }

    func enableTailscaleServe() async {
        hidePairingCode()
        showTailscaleServeConfirmation = false
        tailscaleError = nil
        activeOperation = "Updating Tailscale…"
        defer { activeOperation = nil }
        do {
            tailscaleStatus = try await client.enableTailscaleServe()
            lastError = nil
        } catch {
            tailscaleError = error.localizedDescription
        }
    }

    func disableTailscaleServe() async {
        hidePairingCode()
        tailscaleError = nil
        activeOperation = "Updating Tailscale…"
        defer { activeOperation = nil }
        do {
            tailscaleStatus = try await client.disableTailscaleServe()
            lastError = nil
        } catch {
            tailscaleError = error.localizedDescription
        }
    }

    func openWeb() async {
        pairingError = nil
        do {
            let url = try await client.authenticatedWebURL()
            opener.open(url)
            lastError = nil
        } catch {
            pairingError = error.localizedDescription
        }
    }

    func showPairingCode() async {
        await refresh()
        guard let destination = connectionURL else { return }
        let revision = pairingRevision
        pairingError = nil
        do {
            let url = try await client.pairingURL(for: destination)
            guard destination == connectionURL, revision == pairingRevision else { return }
            pairingExpiryTask?.cancel()
            pairingURL = url
            lastError = nil
            let lifetime = pairingLifetime
            pairingExpiryTask = Task { [weak self] in
                try? await Task.sleep(for: lifetime)
                guard !Task.isCancelled else { return }
                self?.pairingURL = nil
            }
        } catch {
            pairingError = error.localizedDescription
        }
    }

    func hidePairingCode() {
        pairingRevision &+= 1
        pairingExpiryTask?.cancel()
        pairingURL = nil
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

    var lanStateTitle: String {
        if lanURL != nil { return "LAN access is on" }
        guard let address = status?.httpAddr else { return "LAN state unknown" }
        if address.hasPrefix("127.0.0.1:") || address.hasPrefix("[::1]:") {
            return "LAN access is off"
        }
        return "Listener: \(address) — LAN access unknown"
    }

    var connectionURL: URL? {
        if !customPairingOrigin.isEmpty {
            guard let url = URL(string: customPairingOrigin),
                  ["http", "https"].contains(url.scheme ?? ""),
                  let host = url.host?.lowercased(),
                  !["localhost", "localhost.", "0.0.0.0", "::1", "[::1]", "[::]", "::"].contains(host),
                  !host.trimmingCharacters(in: CharacterSet(charactersIn: ".")).hasSuffix(".localhost"),
                  !Self.isMappedLoopbackOrUnspecified(host),
                  !host.hasPrefix("127."),
                  url.user == nil, url.password == nil, url.path.isEmpty,
                  url.query == nil, url.fragment == nil else { return nil }
            return url
        }
        return preferLANPairing ? lanURL : tailscaleStatus?.pairingOrigin ?? lanURL
    }

    private static func isMappedLoopbackOrUnspecified(_ host: String) -> Bool {
        let address = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        var ipv6 = in6_addr()
        guard inet_pton(AF_INET6, address, &ipv6) == 1 else { return false }
        return withUnsafeBytes(of: ipv6) { bytes in
            guard bytes.prefix(10).allSatisfy({ $0 == 0 }),
                  bytes[10] == 255, bytes[11] == 255 else { return false }
            return bytes[12] == 127 || bytes.suffix(4).allSatisfy({ $0 == 0 })
        }
    }

    var isRunning: Bool {
        status?.state == .running || status?.state == .degraded
    }

    var isOperating: Bool {
        activeOperation != nil
    }

    private func runOperation(_ operation: @escaping () async throws -> HolonDaemonStatus) async {
        hidePairingCode()
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
