import Foundation
import ServiceManagement
import Security
import Darwin

enum HolonCLIError: LocalizedError {
    case missingHolonBinary
    case missingTailscaleBinary
    case processFailed(command: [String], terminationStatus: Int32, stderr: String)
    case invalidJSON(String)
    case invalidWebAddress(String)
    case invalidLANTokenFile(String)
    case loginItem(Error)
    case commandLineToolConflict(String)
    case tailscaleServeConflict(String)
    case pairingFailed(String)

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
        case let .invalidLANTokenFile(path):
            return "The control token file must be a nonempty, owner-only regular file: \(path)"
        case let .loginItem(error):
            return "Failed to update the login item: \(error.localizedDescription)"
        case let .commandLineToolConflict(path):
            return "A different holon command already exists at \(path). It was not replaced."
        case let .tailscaleServeConflict(reason):
            return reason
        case let .pairingFailed(reason):
            return reason
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

// UserDefaults supports concurrent access. Store only non-secret approval flags.
private final class HolonAuthenticationPreferences: @unchecked Sendable {
    private let defaults: UserDefaults
    init(_ defaults: UserDefaults) { self.defaults = defaults }
    func string(forKey key: String) -> String? { defaults.string(forKey: key) }
    func set(_ value: String, forKey key: String) { defaults.set(value, forKey: key) }
}

private final class HolonCreatedDaemonProvenance: @unchecked Sendable {
    private let lock = NSLock()
    private var value: HolonDaemonStatus?
    func get() -> HolonDaemonStatus? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
    func set(_ daemon: HolonDaemonStatus?) {
        lock.lock()
        defer { lock.unlock() }
        value = daemon
    }
}

final class HolonCLIClient: HolonDesiredStateClient {
    private let executableURL: URL?
    private let tailscaleExecutableURL: URL?
    private let launcher: HolonProcessLaunching
    private let launchOptions: HolonDaemonLaunchOptions
    private let decoder: JSONDecoder
    private let networkSession: URLSession
    private let preferences: HolonAuthenticationPreferences
    // Session-local provenance; persisted PIDs never authorize authentication changes.
    private let createdDaemon = HolonCreatedDaemonProvenance()

    init(
        executableURL: URL? = nil,
        launcher: HolonProcessLaunching = SystemHolonProcessLauncher(),
        launchOptions: HolonDaemonLaunchOptions = .default,
        tailscaleExecutableURL: URL? = nil,
        networkSession: URLSession = .shared,
        preferences: UserDefaults = .standard
    ) {
        self.executableURL = executableURL
        self.tailscaleExecutableURL = tailscaleExecutableURL
        self.launcher = launcher
        self.launchOptions = launchOptions
        self.networkSession = networkSession
        self.preferences = HolonAuthenticationPreferences(preferences)

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        self.decoder = decoder
    }

    func status() async throws -> HolonDaemonStatus {
        try await run(["daemon", "status"], as: HolonDaemonStatus.self)
    }

    func start() async throws -> HolonDaemonStatus {
        createdDaemon.set(nil)
        let started = try await run(["daemon", "start"] + launchOptionsForCurrentAccess().arguments(), as: HolonDaemonStatus.self)
        // Older CLIs and reused processes do not grant ownership.
        if started.processCreated == true {
            recordManagedDaemon(started)
        }
        return started
    }

    func stop() async throws -> HolonDaemonStatus {
        createdDaemon.set(nil)
        return try await run(["daemon", "stop"], as: HolonDaemonStatus.self)
    }

    func restart() async throws -> HolonDaemonStatus {
        createdDaemon.set(nil)
        let restarted = try await run(["daemon", "restart"] + launchOptionsForCurrentAccess().arguments(), as: HolonDaemonStatus.self)
        recordManagedDaemon(restarted)
        return restarted
    }

    private func launchOptionsForCurrentAccess() async throws -> HolonDaemonLaunchOptions {
        var options = launchOptions
        let currentStatus = try await status()
        if currentStatus.httpAddr.hasPrefix("0.0.0.0:") {
            options.access = "lan"
            options.host = try await localNetworkHost()
            options.listen = "0.0.0.0:\(port(from: currentStatus.httpAddr))"
            options.port = nil
        }
        if canReuseApprovedAuthentication(currentStatus) {
            try loadApprovedAuthentication(&options, homeDir: currentStatus.homeDir)
        }
        return options
    }

    func webURL() async throws -> URL {
        let status = try await status()
        if status.httpAddr.hasPrefix("0.0.0.0:") {
            guard let url = try await lanURL(for: status) else {
                throw HolonCLIError.invalidWebAddress(status.httpAddr)
            }
            return url
        }
        guard let url = status.webURL else {
            throw HolonCLIError.invalidWebAddress(status.httpAddr)
        }
        return url
    }

    func authenticatedWebURL() async throws -> URL {
        let current = try await status()
        let local = loopbackURL(for: current.httpAddr)
        return try await pairingURL(for: local, status: current)
    }

    func pairingURL(for destination: URL) async throws -> URL {
        try await pairingURL(for: destination, status: try await status())
    }

    private func pairingURL(for destination: URL, status: HolonDaemonStatus) async throws -> URL {
        guard status.healthy, status.state == .running || status.state == .degraded else {
            throw HolonCLIError.pairingFailed("Start the Holon daemon before pairing.")
        }
        let local = loopbackURL(for: status.httpAddr)
        var request = URLRequest(url: local.appendingPathComponent("api/auth/pairing/issue"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        // The long-lived token only travels over the loopback request, not in the QR.
        let token = try controlToken(homeDir: status.homeDir)
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token.trimmingCharacters(in: .whitespacesAndNewlines))",
                             forHTTPHeaderField: "Authorization")
        }
        let data: Data
        let statusCode: Int
        if request.value(forHTTPHeaderField: "Authorization") == nil {
            // The Unix control listener admits local callers without a control token.
            let result = try await launcher.run(
                executableURL: URL(fileURLWithPath: "/usr/bin/curl"),
                arguments: ["--silent", "--show-error", "--fail", "--unix-socket",
                            status.socketPath, "--request", "POST",
                            "http://localhost/api/auth/pairing/issue"]
            )
            data = result.stdout
            statusCode = result.terminationStatus == 0 ? 200 : 0
        } else {
            let (body, response) = try await networkSession.data(for: request)
            data = body
            statusCode = (response as? HTTPURLResponse)?.statusCode ?? 0
        }
        guard statusCode == 200 else {
            throw HolonCLIError.pairingFailed(
                "Unable to issue a pairing code. Check daemon version and local token configuration."
            )
        }
        struct Ticket: Decodable { let ticket: String }
        let ticket = try JSONDecoder().decode(Ticket.self, from: data).ticket
        guard !ticket.isEmpty else { throw HolonCLIError.pairingFailed("Empty pairing code.") }
        var components = URLComponents(url: destination, resolvingAgainstBaseURL: false)
        components?.path = "/login"
        components?.query = nil
        components?.fragment = "pair=\(ticket)"
        guard let url = components?.url else {
            throw HolonCLIError.invalidWebAddress(destination.absoluteString)
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
        try await tailscaleServeRequest(method: "GET", action: nil)
    }

    func enableTailscaleServe() async throws -> HolonTailscaleStatus {
        let network = try await tailscaleStatus()
        if network.controlAuthenticationAvailable == false {
            let daemon = try await status()
            guard let created = createdDaemon.get(), daemon.healthy,
                  daemon.controlConnectivity, daemon.pid != nil,
                  daemon.configFingerprintMatch != false,
                  launchOptions.token == nil, launchOptions.tokenFilePath == nil,
                  daemon.pid == created.pid, daemon.homeDir == created.homeDir,
                  daemon.socketPath == created.socketPath,
                  daemon.executablePath == created.executablePath,
                  daemon.httpAddr == created.httpAddr,
                  daemon.runtimeConfigFingerprint == created.runtimeConfigFingerprint,
                  try controlToken(homeDir: daemon.homeDir) == nil else {
                throw HolonCLIError.tailscaleServeConflict(
                    "This daemon is externally managed or its ownership is unknown, or existing credentials conflict with disabled control authentication; an explicit daemon configuration change is required."
                )
            }
            var options = HolonDaemonLaunchOptions()
            // Authentication-only restart inherits network and desktop configuration.
            options.desktopIntegration = nil
            try prepareRemoteAuthentication(&options, homeDir: daemon.homeDir)
            recordAuthenticationApproval(options, homeDir: daemon.homeDir)
            createdDaemon.set(nil)
            let restarted = try await run(
                ["daemon", "restart"] + options.arguments(), as: HolonDaemonStatus.self
            )
            recordManagedDaemon(restarted)
            guard restarted.processCreated == true,
                  restarted.httpAddr == daemon.httpAddr,
                  restarted.homeDir == daemon.homeDir,
                  restarted.healthy, restarted.controlConnectivity,
                  try await tailscaleStatus().controlAuthenticationAvailable == true else {
                throw HolonCLIError.tailscaleServeConflict("Unable to verify restarted daemon authentication before enabling Serve.")
            }
        } else if network.controlAuthenticationAvailable == nil {
            throw HolonCLIError.tailscaleServeConflict("Daemon authentication status is unknown. Configure or update the daemon before enabling Serve.")
        }
        return try await tailscaleServeRequest(method: "POST", action: "enable")
    }

    func disableTailscaleServe() async throws -> HolonTailscaleStatus {
        try await tailscaleServeRequest(method: "POST", action: "disable")
    }

    private func tailscaleServeRequest(method: String, action: String?) async throws -> HolonTailscaleStatus {
        let daemon = try await status()
        let base = loopbackURL(for: daemon.httpAddr)
        var url = base.appendingPathComponent("api/control/network/tailscale/serve")
        if let action { url.appendPathComponent(action) }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let token = try controlToken(homeDir: daemon.homeDir)
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        let (data, response) = try await networkSession.data(for: request)
        guard let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else {
            throw HolonCLIError.tailscaleServeConflict(
                (try? JSONDecoder().decode(HolonServeErrorResponse.self, from: data).error)
                    ?? "Unable to access daemon Tailscale Serve control. Check daemon version and local token configuration."
            )
        }
        let payload = try decoder.decode(HolonTailscaleServeResponse.self, from: data)
        return HolonTailscaleStatus(
            state: !payload.available ? .unavailable : !payload.connected ? .stopped
                : payload.serving ? .serving : .connected,
            hostname: payload.hostname,
            serveURL: payload.serving ? payload.serveUrl.flatMap(URL.init(string:)) : nil,
            message: payload.message,
            desiredEnabled: payload.desiredEnabled,
            serving: payload.serving,
            conflict: payload.conflict,
            statusKnown: payload.statusKnown,
            controlAuthenticationAvailable: payload.controlAuthenticationAvailable
        )
    }

    private func resolveTailscaleBinary() throws -> URL {
        try tailscaleExecutableURL ?? TailscaleBinaryLocator.resolve()
    }

    private func serveConfiguration(executable: URL) async throws -> HolonTailscaleServeConfiguration {
        let arguments = ["serve", "status", "--json"]
        let result = try await launcher.run(executableURL: executable, arguments: arguments)
        guard result.terminationStatus == 0 else {
            throw HolonCLIError.processFailed(
                command: [executable.path] + arguments,
                terminationStatus: result.terminationStatus,
                stderr: String(data: result.stderr, encoding: .utf8) ?? ""
            )
        }
        guard let configuration = try? JSONDecoder().decode(
            HolonTailscaleServeConfiguration.self, from: result.stdout
        ) else {
            throw HolonCLIError.invalidJSON("Tailscale Serve status")
        }
        return configuration
    }

    func lanURL() async throws -> URL? {
        try await lanURL(for: status())
    }

    private func lanURL(for currentStatus: HolonDaemonStatus) async throws -> URL? {
        guard currentStatus.httpAddr.hasPrefix("0.0.0.0:") else {
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
        options.listen = "0.0.0.0:\(port)"
        options.port = nil
        options.advertise = nil
        let network: HolonTailscaleStatus
        do {
            network = try await tailscaleStatus()
        } catch {
            throw HolonCLIError.tailscaleServeConflict(
                "Unable to verify daemon authentication. Configure credentials in its launch settings before enabling LAN: \(error.localizedDescription)"
            )
        }
        guard let authenticated = network.controlAuthenticationAvailable else {
            throw HolonCLIError.tailscaleServeConflict("Daemon authentication status is unknown. Configure or update the daemon before enabling LAN.")
        }
        if !authenticated {
            try prepareRemoteAuthentication(&options, homeDir: currentStatus.homeDir)
            recordAuthenticationApproval(options, homeDir: currentStatus.homeDir)
        } else if canReuseApprovedAuthentication(currentStatus) {
            try loadApprovedAuthentication(&options, homeDir: currentStatus.homeDir)
        }
        let restarted = try await run(
            ["daemon", "restart"] + options.arguments(),
            as: HolonDaemonStatus.self
        )
        recordManagedDaemon(restarted)
        guard let url = URL(string: "http://\(host):\(port)") else {
            throw HolonCLIError.invalidWebAddress("\(host):\(port)")
        }
        return url
    }

    private func prepareRemoteAuthentication(_ options: inout HolonDaemonLaunchOptions, homeDir: String) throws {
        if let token = options.token {
            guard !token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                throw HolonCLIError.invalidLANTokenFile("configured token is empty")
            }
            return
        }
        if let path = options.tokenFilePath {
            try validateRemoteTokenFile(path)
            return
        }
        let path = URL(fileURLWithPath: homeDir, isDirectory: true)
            .appendingPathComponent("control.token").path
        let fd = Darwin.open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, mode_t(0o600))
        if fd >= 0 {
            defer { Darwin.close(fd) }
            do {
                var bytes = [UInt8](repeating: 0, count: 32)
                guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
                    throw HolonCLIError.invalidLANTokenFile(path)
                }
                let token = bytes.map { String(format: "%02x", $0) }.joined()
                try FileHandle(fileDescriptor: fd, closeOnDealloc: false)
                    .write(contentsOf: Data(token.utf8))
            } catch {
                Darwin.unlink(path)
                throw error
            }
        } else if errno != EEXIST {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }

        try validateRemoteTokenFile(path)
        options.tokenFilePath = path
    }

    private func validateRemoteTokenFile(_ path: String) throws {
        _ = try readRemoteTokenFile(path)
    }

    private func readRemoteTokenFile(_ path: String) throws -> String {
        let fd = Darwin.open(path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK)
        guard fd >= 0 else { throw HolonCLIError.invalidLANTokenFile(path) }
        defer { Darwin.close(fd) }
        var info = stat()
        guard fstat(fd, &info) == 0,
              (info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFREG),
              info.st_uid == getuid(),
              (info.st_mode & 0o400) != 0,
              (info.st_mode & 0o077) == 0,
              let data = try? FileHandle(fileDescriptor: fd, closeOnDealloc: false).readToEnd(),
              let token = String(data: data, encoding: .utf8),
              !token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw HolonCLIError.invalidLANTokenFile(path)
        }
        return token.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func controlToken(homeDir: String) throws -> String? {
        if let token = launchOptions.token {
            return token.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        let path = launchOptions.tokenFilePath
            ?? preferences.string(forKey: authenticationApprovalKey(homeDir))
            ?? URL(fileURLWithPath: homeDir).appendingPathComponent("control.token").path
        var info = stat()
        if lstat(path, &info) != 0, errno == ENOENT { return nil }
        return try readRemoteTokenFile(path)
    }

    func disableLAN() async throws -> HolonDaemonStatus {
        var options = launchOptions
        options.access = "local"
        options.host = nil
        options.port = nil
        let currentStatus = try await status()
        options.listen = "127.0.0.1:\(port(from: currentStatus.httpAddr))"
        options.advertise = nil
        if canReuseApprovedAuthentication(currentStatus) {
            try loadApprovedAuthentication(&options, homeDir: currentStatus.homeDir)
        }
        let restarted = try await run(
            ["daemon", "restart"] + options.arguments(),
            as: HolonDaemonStatus.self
        )
        recordManagedDaemon(restarted)
        return restarted
    }

    private func managedDaemonKey(_ homeDir: String) -> String {
        "HolonMenu.managedDaemonPID.\(homeDir)"
    }

    private func canReuseApprovedAuthentication(_ daemon: HolonDaemonStatus) -> Bool {
        guard let pid = daemon.pid else { return true }
        return preferences.string(forKey: managedDaemonKey(daemon.homeDir)) == String(pid)
    }

    private func recordManagedDaemon(_ daemon: HolonDaemonStatus) {
        createdDaemon.set(daemon.processCreated == true ? daemon : nil)
        guard let pid = daemon.pid else { return }
        preferences.set(String(pid), forKey: managedDaemonKey(daemon.homeDir))
    }

    private func authenticationApprovalKey(_ homeDir: String) -> String {
        "HolonMenu.remoteAuthenticationApproved.\(homeDir)"
    }

    private func recordAuthenticationApproval(_ options: HolonDaemonLaunchOptions, homeDir: String) {
        guard let path = options.tokenFilePath else { return }
        preferences.set(path, forKey: authenticationApprovalKey(homeDir))
    }

    private func loadApprovedAuthentication(_ options: inout HolonDaemonLaunchOptions, homeDir: String) throws {
        guard options.token == nil, options.tokenFilePath == nil,
              let path = preferences.string(forKey: authenticationApprovalKey(homeDir)) else { return }
        try validateRemoteTokenFile(path)
        options.tokenFilePath = path
    }

    private func localNetworkHost() async throws -> String {
        for interface in ["en0", "en1"] {
            let result = try? await launcher.run(
                executableURL: URL(fileURLWithPath: "/usr/sbin/ipconfig"),
                arguments: ["getifaddr", interface]
            )
            if let host = result
                .flatMap({ String(data: $0.stdout, encoding: .utf8) })?
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

    private func loopbackURL(for address: String) -> URL {
        let host = address.hasPrefix("[::1]:") || address.hasPrefix("[::]:")
            ? "[::1]" : "127.0.0.1"
        return URL(string: "http://\(host):\(port(from: address))")!
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

private struct HolonTailscaleServeResponse: Decodable {
    let desiredEnabled: Bool
    let available: Bool
    let connected: Bool
    let statusKnown: Bool
    let serving: Bool
    let conflict: Bool
    let hostname: String?
    let serveUrl: String?
    let message: String
    let controlAuthenticationAvailable: Bool?
}

private struct HolonServeErrorResponse: Decodable {
    let error: String
}
