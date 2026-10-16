import Foundation

enum HolonDaemonLifecycleState: String, Codable, Sendable {
    case running
    case degraded
    case stopped
    case stale
    case unresponsive
    case versionMismatch = "version_mismatch"

    var title: String {
        title(locale: nil)
    }

    func title(locale: Locale?) -> String {
        L10n.text(self == .versionMismatch ? "Version mismatch" : rawValue.capitalized, locale: locale)
    }
}

struct HolonDaemonStatus: Codable, Equatable, Sendable {
    var ok: Bool
    var state: HolonDaemonLifecycleState
    var healthy: Bool
    var homeDir: String
    var socketPath: String
    var httpAddr: String
    var webUrl: String?
    var productVersion: String?
    var controlProtocolVersion: UInt32?
    var lifecycleOwner: String?
    var executablePath: String?
    var desiredRunning: Bool
    var pid: UInt32?
    var controlConnectivity: Bool
    var runtimeConfigFingerprint: String?
    var configFingerprintMatch: Bool?
    var message: String
    var processCreated: Bool? = nil
    var desiredServe: HolonDesiredServeProfile? = nil

    var webURL: URL? {
        HolonURLBuilder.webURL(from: webUrl ?? httpAddr)
    }

    var logURL: URL {
        URL(fileURLWithPath: homeDir, isDirectory: true)
            .appendingPathComponent("run")
            .appendingPathComponent("daemon.log")
    }
}

// Read-only status projection; the daemon owns the full persisted profile.
struct HolonDesiredServeProfile: Codable, Equatable, Sendable {
    var source: String?
    var desktopIntegration: Bool?
}

struct HolonDaemonLogs: Codable, Equatable, Sendable {
    var ok: Bool
    var logPath: String
    var tail: [String]
    var message: String

    var fileURL: URL {
        URL(fileURLWithPath: logPath)
    }
}

enum HolonTailscaleState: String, Equatable, Sendable {
    case unavailable
    case stopped
    case loggedOut = "logged_out"
    case connected
    case serving
    case unknown
}

struct HolonTailscaleStatus: Equatable, Sendable {
    var state: HolonTailscaleState
    var hostname: String?
    var serveURL: URL?
    var message: String
    var desiredEnabled = false
    var serving = false
    var conflict = false
    var statusKnown = false
    var controlAuthenticationAvailable: Bool? = nil
    var details: String? = nil
    var available = false
    var connected = false

    var canEnableServe: Bool {
        state == .connected && available && connected && statusKnown && !conflict
    }

    var pairingOrigin: URL? {
        guard state == .serving, statusKnown, serving, !conflict,
              let hostname, let url = serveURL,
              url.scheme == "https", url.host?.lowercased() == hostname.lowercased(),
              url.user == nil, url.password == nil, url.port == nil || url.port == 443,
              url.path.isEmpty, url.query == nil, url.fragment == nil else { return nil }
        return url
    }

    var hasDrift: Bool { statusKnown && desiredEnabled != serving }

    var desiredTitle: String { L10n.format("Desired: %@", desiredEnabled ? L10n.text("On") : L10n.text("Off")) }
    var actualTitle: String {
        statusKnown ? L10n.format("Actual: %@", serving ? L10n.text("Serving") : L10n.text("Not serving")) : L10n.text("Actual: Unknown")
    }

    var title: String {
        switch state {
        case .unavailable:
            return L10n.text("CLI not found (optional)")
        case .stopped:
            return L10n.text("Not running")
        case .loggedOut:
            return L10n.text("Sign-in required")
        case .connected:
            return L10n.text("Connected")
        case .serving:
            return L10n.text("Serve enabled")
        case .unknown:
            return L10n.text("Status unknown")
        }
    }

    static func parse(
        statusOutput: String,
        serveOutput: String = "",
        holonURL: URL? = nil
    ) -> Self {
        guard let status = try? JSONDecoder().decode(
            HolonTailscaleProbeResponse.self, from: Data(statusOutput.utf8)
        ) else {
            return Self(
                state: .unknown, hostname: nil, serveURL: nil,
                message: L10n.text("Status unknown"), available: true
            )
        }
        let hostname = status.selfStatus?.dnsName
            .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: ".")) }
            .flatMap { $0.isEmpty ? nil : $0 }
        let configuration = try? JSONDecoder().decode(
            HolonTailscaleServeConfiguration.self, from: Data(serveOutput.utf8)
        )
        let serveURL: URL? = if let hostname, let holonURL,
                                configuration?.rootProxy(for: hostname) == holonURL {
            URL(string: "https://\(hostname)")
        } else {
            nil
        }

        if status.backendState == "Running", let serveURL {
            return Self(
                state: .serving,
                hostname: hostname,
                serveURL: serveURL,
                message: L10n.text("Tailscale Serve is exposing Holon."),
                serving: true,
                statusKnown: true,
                available: true,
                connected: true
            )
        }
        if status.backendState == "NeedsLogin" || status.backendState == "NeedsMachineAuth" {
            return Self(
                state: .loggedOut,
                hostname: hostname,
                serveURL: nil,
                message: L10n.text("Sign in to Tailscale before enabling Serve."),
                available: true
            )
        }
        if let backendState = status.backendState, ["Stopped", "NoState", "Starting"].contains(backendState) {
            return Self(
                state: .stopped,
                hostname: hostname,
                serveURL: nil,
                message: L10n.text("Start Tailscale before enabling Serve."),
                available: true
            )
        }
        guard status.backendState == "Running" else {
            return Self(
                state: .unknown, hostname: hostname, serveURL: nil,
                message: L10n.text("Status unknown"), available: true
            )
        }
        return Self(
            state: .connected,
            hostname: hostname,
            serveURL: nil,
            message: L10n.text("Tailscale is connected; Serve is not enabled."),
            conflict: hostname.map { configuration?.hasRootHandler(for: $0) == true } ?? false,
            statusKnown: configuration != nil,
            available: true,
            connected: true
        )
    }
}

struct HolonTailscaleServeConfiguration: Decodable {
    let web: [String: Web]?

    struct Web: Decodable {
        let handlers: [String: Handler]?
        enum CodingKeys: String, CodingKey { case handlers = "Handlers" }
    }

    struct Handler: Decodable {
        let proxy: String?
        enum CodingKeys: String, CodingKey { case proxy = "Proxy" }
    }

    enum CodingKeys: String, CodingKey { case web = "Web" }

    func hasRootHandler(for hostname: String) -> Bool {
        web?["\(hostname):443"]?.handlers?["/"] != nil
    }

    func rootProxy(for hostname: String) -> URL? {
        web?["\(hostname):443"]?.handlers?["/"]?.proxy.flatMap(URL.init(string:))
    }
}

private struct HolonTailscaleProbeResponse: Decodable {
    let backendState: String?
    let selfStatus: SelfStatus?

    struct SelfStatus: Decodable {
        let dnsName: String?
        enum CodingKeys: String, CodingKey { case dnsName = "DNSName" }
    }

    enum CodingKeys: String, CodingKey {
        case backendState = "BackendState"
        case selfStatus = "Self"
    }
}

struct HolonDaemonLaunchOptions: Equatable, Sendable {
    var access: String?
    var host: String?
    var listen: String?
    var port: UInt16?
    var advertise: String?
    var token: String?
    var tokenFilePath: String?
    var webDistPath: String?
    var desktopIntegration: Bool?

    static let `default` = HolonDaemonLaunchOptions()

    func arguments() -> [String] {
        var arguments: [String] = []

        if let access {
            arguments += ["--access", access]
        }
        if let host {
            arguments += ["--host", host]
        }
        if let listen {
            arguments += ["--listen", listen]
        }
        if let port, listen == nil {
            arguments += ["--port", String(port)]
        }
        if let advertise {
            arguments += ["--advertise", advertise]
        }
        if let token {
            arguments += ["--token", token]
        }
        if let tokenFilePath {
            arguments += ["--token-file", tokenFilePath]
        }
        if let webDistPath {
            arguments += ["--web-dist", webDistPath]
        }

        if let desktopIntegration {
            arguments.append(desktopIntegration ? "--desktop-integration" : "--desktop-integration=false")
        }
        return arguments
    }
}

enum HolonURLBuilder {
    static func webURL(from address: String) -> URL? {
        let string = address.hasPrefix("http://") || address.hasPrefix("https://")
            ? address
            : "http://\(address)"
        return URL(string: string)
    }
}

protocol HolonDesiredStateClient: Sendable {
    func status() async throws -> HolonDaemonStatus
    func start() async throws -> HolonDaemonStatus
    func stop() async throws -> HolonDaemonStatus
    func restart() async throws -> HolonDaemonStatus
    func webURL() async throws -> URL
    func authenticatedWebURL() async throws -> URL
    func pairingURL(for destination: URL) async throws -> URL
    func logsURL() async throws -> URL
    func launchAtLoginEnabled() async throws -> Bool
    func setLaunchAtLoginEnabled(_ enabled: Bool) async throws
    func installCommandLineTool() async throws -> URL
    func tailscaleStatus() async throws -> HolonTailscaleStatus
    func enableTailscaleServe() async throws -> HolonTailscaleStatus
    func disableTailscaleServe() async throws -> HolonTailscaleStatus
    func lanURL() async throws -> URL?
    func enableLAN() async throws -> URL
    func disableLAN() async throws -> HolonDaemonStatus
}
