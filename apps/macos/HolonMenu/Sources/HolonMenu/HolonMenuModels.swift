import Foundation

enum HolonDaemonLifecycleState: String, Codable, Sendable {
    case running
    case degraded
    case stopped
    case stale
    case unresponsive
    case versionMismatch = "version_mismatch"

    var title: String {
        rawValue.capitalized
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

    var webURL: URL? {
        HolonURLBuilder.webURL(from: webUrl ?? httpAddr)
    }

    var logURL: URL {
        URL(fileURLWithPath: homeDir, isDirectory: true)
            .appendingPathComponent("run")
            .appendingPathComponent("daemon.log")
    }
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

enum HolonTailscaleState: Equatable, Sendable {
    case unavailable
    case stopped
    case loggedOut
    case connected
    case serving
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

    var hasDrift: Bool { statusKnown && desiredEnabled != serving }

    var desiredTitle: String { "Desired: \(desiredEnabled ? "On" : "Off")" }
    var actualTitle: String {
        statusKnown ? "Actual: \(serving ? "Serving" : "Not serving")" : "Actual: Unknown"
    }

    var title: String {
        switch state {
        case .unavailable:
            return "Not installed"
        case .stopped:
            return "Not running"
        case .loggedOut:
            return "Sign-in required"
        case .connected:
            return "Connected"
        case .serving:
            return "Serve enabled"
        }
    }

    static func parse(
        statusOutput: String,
        serveOutput: String = "",
        holonURL: URL? = nil
    ) -> Self {
        let status = statusOutput.lowercased()
        let hostname = statusOutput
            .firstMatch(of: #""DNSName"\s*:\s*"([^"]+)"#)
            .flatMap { $0.split(separator: "\"").last.map(String.init) }
            .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: ".")) }
        let configuration = try? JSONDecoder().decode(
            HolonTailscaleServeConfiguration.self, from: Data(serveOutput.utf8)
        )
        let serveURL: URL? = if let hostname, let holonURL,
                                configuration?.rootProxy(for: hostname) == holonURL {
            URL(string: "https://\(hostname)")
        } else {
            nil
        }

        if let serveURL {
            return Self(
                state: .serving,
                hostname: hostname,
                serveURL: serveURL,
                message: "Tailscale Serve is exposing Holon."
            )
        }
        if status.contains("needslogin") || status.contains("logged out") {
            return Self(
                state: .loggedOut,
                hostname: hostname,
                serveURL: nil,
                message: "Sign in to Tailscale before enabling Serve."
            )
        }
        if status.contains("stopped") {
            return Self(
                state: .stopped,
                hostname: hostname,
                serveURL: nil,
                message: "Start Tailscale before enabling Serve."
            )
        }
        return Self(
            state: .connected,
            hostname: hostname,
            serveURL: nil,
            message: "Tailscale is connected; Serve is not enabled."
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

private extension String {
    func firstMatch(of pattern: String) -> String? {
        guard let range = range(of: pattern, options: .regularExpression) else {
            return nil
        }
        return String(self[range])
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
        if let port {
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

        // The menu app runs alongside its managed daemon on this Mac.
        // Restarts inherit the previous daemon's flags; LAN must explicitly turn this off.
        arguments.append(access == "lan" ? "--desktop-integration=false" : "--desktop-integration")
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
