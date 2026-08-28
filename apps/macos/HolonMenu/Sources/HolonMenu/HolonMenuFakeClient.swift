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
    }

    private(set) var commands: [Command] = []
    private var currentStatus: HolonDaemonStatus
    private var launchAtLoginEnabledValue: Bool

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
        launchAtLoginEnabled: Bool = false
    ) {
        self.currentStatus = currentStatus
        self.launchAtLoginEnabledValue = launchAtLoginEnabled
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

    func recordedCommands() -> [Command] {
        commands
    }
}
