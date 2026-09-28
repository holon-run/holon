import Foundation

actor FakeHolonClient: HolonDesiredStateClient {
    enum Command: Equatable, Sendable {
        case status
        case start
        case stop
        case restart
        case webURL
        case logsURL
        case launchAtLoginEnabled
        case setLaunchAtLoginEnabled(Bool)
        case installCommandLineTool
        case tailscaleStatus
        case enableTailscaleServe
        case disableTailscaleServe
        case lanURL
        case enableLAN
        case disableLAN
    }

    private(set) var commands: [Command] = []
    private var currentStatus: HolonDaemonStatus
    private var launchAtLoginEnabledValue: Bool
    private var tailscaleStatusValue: HolonTailscaleStatus
    private var enableLANError: Error?
    private var enableTailscaleServeError: Error?

    init(
        currentStatus: HolonDaemonStatus = HolonDaemonStatus(
            ok: true,
            state: .stopped,
            healthy: false,
            homeDir: "/Users/holon/.holon",
            socketPath: "/tmp/holon.sock",
            httpAddr: "127.0.0.1:7878",
            webUrl: "http://127.0.0.1:7878",
            productVersion: "test",
            controlProtocolVersion: 1,
            lifecycleOwner: "standalone",
            executablePath: "/Applications/Holon.app/Contents/Resources/bin/holon",
            desiredRunning: false,
            pid: nil,
            controlConnectivity: false,
            runtimeConfigFingerprint: nil,
            configFingerprintMatch: nil,
            message: "Holon runtime is stopped."
        ),
        launchAtLoginEnabled: Bool = false,
        enableLANError: Error? = nil,
        enableTailscaleServeError: Error? = nil,
        tailscaleStatus: HolonTailscaleStatus = HolonTailscaleStatus(
            state: .connected,
            hostname: "holon.example.ts.net",
            serveURL: nil,
            message: "Tailscale is connected; Serve is not enabled."
        )
    ) {
        self.currentStatus = currentStatus
        self.launchAtLoginEnabledValue = launchAtLoginEnabled
        self.tailscaleStatusValue = tailscaleStatus
        self.enableLANError = enableLANError
        self.enableTailscaleServeError = enableTailscaleServeError
    }

    func status() async throws -> HolonDaemonStatus {
        commands.append(.status)
        return currentStatus
    }

    func start() async throws -> HolonDaemonStatus {
        commands.append(.start)
        currentStatus.state = .running
        currentStatus.healthy = true
        currentStatus.ok = true
        currentStatus.desiredRunning = true
        currentStatus.message = "Holon runtime is running."
        return currentStatus
    }

    func stop() async throws -> HolonDaemonStatus {
        commands.append(.stop)
        currentStatus.state = .stopped
        currentStatus.healthy = false
        currentStatus.ok = true
        currentStatus.desiredRunning = false
        currentStatus.message = "Holon runtime is stopped."
        return currentStatus
    }

    func restart() async throws -> HolonDaemonStatus {
        commands.append(.restart)
        currentStatus.state = .running
        currentStatus.healthy = true
        currentStatus.ok = true
        currentStatus.desiredRunning = true
        currentStatus.message = "Holon runtime restarted."
        return currentStatus
    }

    func webURL() async throws -> URL {
        commands.append(.webURL)
        guard let url = currentStatus.webURL else {
            throw HolonCLIError.invalidWebAddress(currentStatus.httpAddr)
        }
        return url
    }

    func logsURL() async throws -> URL {
        commands.append(.logsURL)
        return currentStatus.logURL
    }

    func launchAtLoginEnabled() async throws -> Bool {
        commands.append(.launchAtLoginEnabled)
        return launchAtLoginEnabledValue
    }

    func setLaunchAtLoginEnabled(_ enabled: Bool) async throws {
        commands.append(.setLaunchAtLoginEnabled(enabled))
        launchAtLoginEnabledValue = enabled
    }

    func installCommandLineTool() async throws -> URL {
        commands.append(.installCommandLineTool)
        return URL(fileURLWithPath: "/Users/holon/.local/bin/holon")
    }

    func tailscaleStatus() async throws -> HolonTailscaleStatus {
        commands.append(.tailscaleStatus)
        return tailscaleStatusValue
    }

    func enableTailscaleServe() async throws -> HolonTailscaleStatus {
        commands.append(.enableTailscaleServe)
        if let enableTailscaleServeError {
            throw enableTailscaleServeError
        }
        tailscaleStatusValue = HolonTailscaleStatus(
            state: .serving,
            hostname: tailscaleStatusValue.hostname,
            serveURL: URL(string: "https://holon.example.ts.net"),
            message: "Tailscale Serve is exposing Holon."
        )
        return tailscaleStatusValue
    }

    func disableTailscaleServe() async throws -> HolonTailscaleStatus {
        commands.append(.disableTailscaleServe)
        tailscaleStatusValue = HolonTailscaleStatus(
            state: .connected,
            hostname: tailscaleStatusValue.hostname,
            serveURL: nil,
            message: "Tailscale is connected; Serve is not enabled."
        )
        return tailscaleStatusValue
    }

    func lanURL() async throws -> URL? {
        commands.append(.lanURL)
        return currentStatus.httpAddr.hasPrefix("127.")
            ? nil
            : URL(string: "http://192.168.1.20:7878")
    }

    func enableLAN() async throws -> URL {
        commands.append(.enableLAN)
        if let enableLANError {
            throw enableLANError
        }
        currentStatus.httpAddr = "0.0.0.0:7878"
        currentStatus.webUrl = "http://127.0.0.1:7878"
        return URL(string: "http://192.168.1.20:7878")!
    }

    func disableLAN() async throws -> HolonDaemonStatus {
        commands.append(.disableLAN)
        currentStatus.httpAddr = "127.0.0.1:7878"
        currentStatus.webUrl = "http://127.0.0.1:7878"
        return currentStatus
    }

    func recordedCommands() -> [Command] {
        commands
    }
}
