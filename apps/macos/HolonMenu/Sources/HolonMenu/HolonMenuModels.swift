import Foundation

enum HolonDaemonLifecycleState: String, Codable, Sendable {
    case running
    case degraded
    case stopped
    case stale
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
    func logsURL() async throws -> URL
    func launchAtLoginEnabled() async throws -> Bool
    func setLaunchAtLoginEnabled(_ enabled: Bool) async throws
    func installCommandLineTool() async throws -> URL
}
