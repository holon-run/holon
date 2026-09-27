import Foundation
import ServiceManagement

enum HolonCLIError: LocalizedError {
    case missingHolonBinary
    case missingTailscaleBinary
    case processFailed(command: [String], terminationStatus: Int32, stderr: String)
    case invalidJSON(String)
    case invalidWebAddress(String)
    case loginItem(Error)
    case commandLineToolConflict(String)

    var errorDescription: String? {
        switch self {
        case .missingHolonBinary:
            return "Unable to locate the bundled holon CLI."
        case .missingTailscaleBinary:
            return "Unable to locate the Tailscale command-line tool."
        case let .processFailed(command, terminationStatus, stderr):
            let renderedCommand = command.joined(separator: " ")
            if stderr.isEmpty {
                return "holon command failed with exit status \(terminationStatus): \(renderedCommand)"
            }
            return "holon command failed with exit status \(terminationStatus): \(renderedCommand)\n\(stderr)"
        case let .invalidJSON(output):
            return "holon returned invalid JSON: \(output)"
        case let .invalidWebAddress(address):
            return "holon returned an invalid web address: \(address)"
        case let .loginItem(error):
            return "Failed to update the login item: \(error.localizedDescription)"
        case let .commandLineToolConflict(path):
            return "A different holon command already exists at \(path). It was not replaced."
        }
    }
}

struct HolonProcessResult: Sendable {
    var terminationStatus: Int32
    var stdout: Data
    var stderr: Data
}

protocol HolonProcessLaunching: Sendable {
    func run(executableURL: URL, arguments: [String]) async throws -> HolonProcessResult
}

struct SystemHolonProcessLauncher: HolonProcessLaunching {
    func run(executableURL: URL, arguments: [String]) async throws -> HolonProcessResult {
        try await Task.detached(priority: .utility) {
            let process = Process()
            process.executableURL = executableURL
            process.arguments = arguments

            let stdoutPipe = Pipe()
            let stderrPipe = Pipe()
            process.standardOutput = stdoutPipe
            process.standardError = stderrPipe

            try process.run()
            process.waitUntilExit()

            let stdout = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
            let stderr = stderrPipe.fileHandleForReading.readDataToEndOfFile()
            return HolonProcessResult(
                terminationStatus: process.terminationStatus,
                stdout: stdout,
                stderr: stderr
            )
        }.value
    }
}

enum HolonBinaryLocator {
    static func resolve() throws -> URL {
        if let path = ProcessInfo.processInfo.environment["HOLON_BINARY_PATH"], !path.isEmpty {
            return URL(fileURLWithPath: path)
        }

        let bundle = Bundle.main
        if let url = bundle.url(forAuxiliaryExecutable: "holon") {
            return url
        }
        if let url = bundle.url(forResource: "holon", withExtension: nil) {
            return url
        }
        if let resourceURL = bundle.resourceURL {
            let candidate = resourceURL.appendingPathComponent("bin/holon")
            if FileManager.default.isExecutableFile(atPath: candidate.path) {
                return candidate
            }
        }
        if let executable = bundle.executableURL {
            let candidate = executable.deletingLastPathComponent().appendingPathComponent("holon")
            if FileManager.default.fileExists(atPath: candidate.path) {
                return candidate
            }
        }

        throw HolonCLIError.missingHolonBinary
    }
}

enum TailscaleBinaryLocator {
    static func resolve() throws -> URL {
        if let path = ProcessInfo.processInfo.environment["TAILSCALE_BINARY_PATH"], !path.isEmpty {
            let url = URL(fileURLWithPath: path)
            if FileManager.default.isExecutableFile(atPath: url.path) {
                return url
            }
        }

        let candidates = [
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
            "/usr/bin/tailscale",
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        ]
        if let path = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) {
            return URL(fileURLWithPath: path)
        }
        throw HolonCLIError.missingTailscaleBinary
    }
}

final class HolonCLIClient: HolonDesiredStateClient {
    private let executableURL: URL?
    private let launcher: HolonProcessLaunching
    private let launchOptions: HolonDaemonLaunchOptions
    private let decoder: JSONDecoder

    init(
        executableURL: URL? = nil,
        launcher: HolonProcessLaunching = SystemHolonProcessLauncher(),
        launchOptions: HolonDaemonLaunchOptions = .default
    ) {
        self.executableURL = executableURL
        self.launcher = launcher
        self.launchOptions = launchOptions

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        self.decoder = decoder
    }

    func status() async throws -> HolonDaemonStatus {
        try await run(["daemon", "status"], as: HolonDaemonStatus.self)
    }

    func start() async throws -> HolonDaemonStatus {
        try await run(["daemon", "start"] + launchOptions.arguments(), as: HolonDaemonStatus.self)
    }

    func stop() async throws -> HolonDaemonStatus {
        try await run(["daemon", "stop"], as: HolonDaemonStatus.self)
    }

    func restart() async throws -> HolonDaemonStatus {
        try await run(["daemon", "restart"] + launchOptions.arguments(), as: HolonDaemonStatus.self)
    }

    func webURL() async throws -> URL {
        let status = try await status()
        guard let url = status.webURL else {
            throw HolonCLIError.invalidWebAddress(status.httpAddr)
        }
        return url
    }

    func logsURL() async throws -> URL {
        let logs = try await run(["daemon", "logs", "--tail", "1"], as: HolonDaemonLogs.self)
        return logs.fileURL
    }

    func launchAtLoginEnabled() async throws -> Bool {
        SMAppService.mainApp.status == .enabled
    }

    func setLaunchAtLoginEnabled(_ enabled: Bool) async throws {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try await SMAppService.mainApp.unregister()
            }
        } catch {
            throw HolonCLIError.loginItem(error)
        }
    }

    func installCommandLineTool() async throws -> URL {
        let executable = try executableURL ?? HolonBinaryLocator.resolve()
        let destination = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/bin/holon")
        try FileManager.default.createDirectory(
            at: destination.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )

        if FileManager.default.fileExists(atPath: destination.path) {
            let existing = try? FileManager.default.destinationOfSymbolicLink(
                atPath: destination.path
            )
            if existing == executable.path {
                return destination
            }
            throw HolonCLIError.commandLineToolConflict(destination.path)
        }

        try FileManager.default.createSymbolicLink(
            at: destination,
            withDestinationURL: executable
        )
        return destination
    }

    func tailscaleStatus() async throws -> HolonTailscaleStatus {
        guard let executable = try? TailscaleBinaryLocator.resolve() else {
            return HolonTailscaleStatus(
                state: .unavailable,
                hostname: nil,
                serveURL: nil,
                message: "Install Tailscale to enable a tailnet connection."
            )
        }
        guard let status = try? await launcher.run(
            executableURL: executable,
            arguments: ["status", "--json"]
        ) else {
            return HolonTailscaleStatus(
                state: .stopped,
                hostname: nil,
                serveURL: nil,
                message: "Start Tailscale before enabling Serve."
            )
        }
        guard status.terminationStatus == 0 else {
            return HolonTailscaleStatus(
                state: .stopped,
                hostname: nil,
                serveURL: nil,
                message: String(data: status.stderr, encoding: .utf8)
                    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .flatMap { $0.isEmpty ? nil : $0 }
                    ?? "Start Tailscale before enabling Serve."
            )
        }
        let serve = try? await launcher.run(executableURL: executable, arguments: ["serve", "status"])
        return HolonTailscaleStatus.parse(
            statusOutput: String(data: status.stdout, encoding: .utf8) ?? "",
            serveOutput: String(data: serve?.stdout ?? Data(), encoding: .utf8) ?? ""
        )
    }

    func enableTailscaleServe() async throws -> HolonTailscaleStatus {
        let executable = try TailscaleBinaryLocator.resolve()
        let url = try await webURL()
        let result = try await launcher.run(
            executableURL: executable,
            arguments: ["serve", "--bg", url.absoluteString]
        )
        guard result.terminationStatus == 0 else {
            throw HolonCLIError.processFailed(
                command: [executable.path, "serve", "--bg", url.absoluteString],
                terminationStatus: result.terminationStatus,
                stderr: String(data: result.stderr, encoding: .utf8) ?? ""
            )
        }
        return try await tailscaleStatus()
    }

    func disableTailscaleServe() async throws -> HolonTailscaleStatus {
        let executable = try TailscaleBinaryLocator.resolve()
        let result = try await launcher.run(
            executableURL: executable,
            arguments: ["serve", "reset"]
        )
        guard result.terminationStatus == 0 else {
            throw HolonCLIError.processFailed(
                command: [executable.path, "serve", "reset"],
                terminationStatus: result.terminationStatus,
                stderr: String(data: result.stderr, encoding: .utf8) ?? ""
            )
        }
        return try await tailscaleStatus()
    }

    func lanURL() async throws -> URL? {
        let currentStatus = try await status()
        guard isNonLoopbackAddress(currentStatus.httpAddr) else {
            return nil
        }
        let host = try await localNetworkHost()
        return URL(string: "http://\(host):\(port(from: currentStatus.httpAddr))")
    }

    func enableLAN() async throws -> URL {
        let currentStatus = try await status()
        let host = try await localNetworkHost()
        let port = port(from: currentStatus.httpAddr)
        var options = launchOptions
        options.access = "lan"
        options.host = host
        options.port = port
        options.advertise = nil
        _ = try await run(
            ["daemon", "restart"] + options.arguments(),
            as: HolonDaemonStatus.self
        )
        guard let url = URL(string: "http://\(host):\(port)") else {
            throw HolonCLIError.invalidWebAddress("\(host):\(port)")
        }
        return url
    }

    func disableLAN() async throws -> HolonDaemonStatus {
        var options = launchOptions
        options.access = "local"
        options.advertise = nil
        return try await run(
            ["daemon", "restart"] + options.arguments(),
            as: HolonDaemonStatus.self
        )
    }

    private func localNetworkHost() async throws -> String {
        for interface in ["en0", "en1"] {
            let result = try? await launcher.run(
                executableURL: URL(fileURLWithPath: "/sbin/ipconfig"),
                arguments: ["getifaddr", interface]
            )
            if let host = result?.stdout
                .flatMap({ String(data: $0, encoding: .utf8) })?
                .trimmingCharacters(in: .whitespacesAndNewlines),
               isIPv4Address(host) {
                return host
            }
        }
        throw HolonCLIError.invalidWebAddress(
            "Unable to determine the Mac's local network address."
        )
    }

    private func port(from address: String) -> UInt16 {
        UInt16(address.split(separator: ":").last ?? "7878") ?? 7878
    }

    private func isNonLoopbackAddress(_ address: String) -> Bool {
        let host = address.split(separator: ":").first.map(String.init) ?? address
        return host != "127.0.0.1" && host != "localhost" && host != "[::1]"
    }

    private func isIPv4Address(_ value: String) -> Bool {
        let octets = value.split(separator: ".")
        return octets.count == 4 && octets.allSatisfy {
            guard let number = Int($0) else { return false }
            return (0...255).contains(number)
        }
    }

    private func run<T: Decodable>(_ arguments: [String], as type: T.Type) async throws -> T {
        let executable = try executableURL ?? HolonBinaryLocator.resolve()
        let result = try await launcher.run(executableURL: executable, arguments: arguments)

        guard result.terminationStatus == 0 else {
            throw HolonCLIError.processFailed(
                command: [executable.path] + arguments,
                terminationStatus: result.terminationStatus,
                stderr: String(data: result.stderr, encoding: .utf8) ?? ""
            )
        }

        do {
            return try decoder.decode(T.self, from: result.stdout)
        } catch {
            throw HolonCLIError.invalidJSON(
                String(data: result.stdout, encoding: .utf8) ?? "<non-utf8 output>"
            )
        }
    }
}
