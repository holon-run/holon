import Foundation
import XCTest
@testable import HolonMenu

final class HolonMenuServeProfileTests: XCTestCase {
    private func statusResult(_ fields: String = "") -> HolonProcessResult {
        HolonProcessResult(
            terminationStatus: 0,
            stdout: Data("""
                {"ok":true,"state":"stopped","healthy":false,"home_dir":"/tmp/holon",
                "socket_path":"/tmp/holon.sock","http_addr":"0.0.0.0:9000",
                "desired_running":false,"control_connectivity":false,"message":"Stopped"
                \(fields)}
                """.utf8),
            stderr: Data()
        )
    }

    func testLifecycleSendsConditionalDesktopHintOnlyForExplicitDefaults() async throws {
        let cases: [(
            fields: String, source: String?, desktop: Bool?, hasDesiredServe: Bool, requestsHint: Bool
        )] = [
            ("", nil, nil, false, false),
            (#","desired_serve":null"#, nil, nil, false, false),
            (#","desired_serve_source":"defaults""#, nil, nil, false, false),
            (#","desired_serve":{}"#, nil, nil, true, false),
            (#","desired_serve":{"desktop_integration":false}"#, nil, false, true, false),
            (#","desired_serve":{"source":null,"desktop_integration":false}"#,
             nil, false, true, false),
            (#","desired_serve":{"source":"profile","desktop_integration":false}"#,
             "profile", false, true, false),
            (#","desired_serve":{"source":"legacy","desktop_integration":false}"#,
             "legacy", false, true, false),
            (#","desired_serve":{"source":"profile","desktop_integration":true}"#,
             "profile", true, true, false),
            (#","desired_serve":{"source":"legacy","desktop_integration":true}"#,
             "legacy", true, true, false),
            (#","desired_serve":{"source":"future_source","desktop_integration":false}"#,
             "future_source", false, true, false),
            (#","desired_serve":{"source":"defaults"}"#, "defaults", nil, true, true),
            (#","desired_serve":{"source":"defaults","desktop_integration":false}"#,
             "defaults", false, true, true),
        ]
        for item in cases {
            for action in ["start", "restart"] {
                let launcher = RecordingProcessLauncher(result: .success(statusResult(item.fields)))
                let client = HolonCLIClient(
                    executableURL: URL(fileURLWithPath: "/opt/holon"), launcher: launcher
                )
                let status = try await client.status()
                XCTAssertEqual(status.desiredServe?.source, item.source)
                XCTAssertEqual(status.desiredServe?.desktopIntegration, item.desktop)
                XCTAssertEqual(status.desiredServe != nil, item.hasDesiredServe)
                if action == "start" {
                    _ = try await client.start()
                } else {
                    _ = try await client.restart()
                }
                let calls = await launcher.invocations()
                XCTAssertEqual(
                    calls.last?.arguments,
                    ["daemon", action] + (item.requestsHint ? ["--desktop-integration-if-unconfigured"] : []),
                    item.fields
                )
                XCTAssertFalse(calls.contains { $0.arguments.first == "getifaddr" })
            }
        }
    }

    func testConditionalDesktopHintIsNotReplayedWhenProfileExists() async throws {
        let launcher = RecordingProcessLauncher(result: .success(statusResult(
            #","desired_serve":{"source":"defaults"}"#
        )))
        let client = HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"), launcher: launcher
        )
        _ = try await client.start()
        var calls = await launcher.invocations()
        XCTAssertEqual(calls.last?.arguments, ["daemon", "start", "--desktop-integration-if-unconfigured"])

        for desktop in [true, false] {
            await launcher.replaceResult(statusResult(
                #","desired_serve":{"source":"profile","desktop_integration":\#(desktop)}"#
            ))
            _ = try await client.restart()
            calls = await launcher.invocations()
            XCTAssertEqual(calls.last?.arguments, ["daemon", "restart"])
        }
    }

    func testExplicitDesktopSettingSkipsConditionalHintAndStatusProbe() async throws {
        for desktop in [true, false] {
            for action in ["start", "restart"] {
                let launcher = RecordingProcessLauncher(result: .success(statusResult(
                    #","desired_serve":{"source":"defaults"}"#
                )))
                let client = HolonCLIClient(
                    executableURL: URL(fileURLWithPath: "/opt/holon"), launcher: launcher,
                    launchOptions: HolonDaemonLaunchOptions(desktopIntegration: desktop)
                )
                if action == "start" {
                    _ = try await client.start()
                } else {
                    _ = try await client.restart()
                }
                let calls = await launcher.invocations()
                XCTAssertEqual(calls.map(\.arguments), [[
                    "daemon", action, desktop ? "--desktop-integration" : "--desktop-integration=false",
                ]])
            }
        }
    }

    func testLaunchPreservesDisabledDesktopWhenHistoryAppearsAfterStatusSnapshot() async throws {
        for action in ["start", "restart"] {
            let launcher = HistoryAppearingProcessLauncher(
                defaultsSnapshot: statusResult(#","desired_serve":{"source":"defaults"}"#),
                savedProfile: statusResult(
                    #","desired_serve":{"source":"profile","desktop_integration":false}"#
                )
            )
            let client = HolonCLIClient(
                executableURL: URL(fileURLWithPath: "/opt/holon"), launcher: launcher
            )
            let started: HolonDaemonStatus
            if action == "start" {
                started = try await client.start()
            } else {
                started = try await client.restart()
            }
            let calls = await launcher.invocations()
            XCTAssertEqual(calls, [
                ["daemon", "status"],
                ["daemon", action, "--desktop-integration-if-unconfigured"],
            ])
            XCTAssertEqual(started.desiredServe?.source, "profile")
            XCTAssertEqual(started.desiredServe?.desktopIntegration, false)
        }
    }
}

private actor HistoryAppearingProcessLauncher: HolonProcessLaunching {
    private let defaultsSnapshot: HolonProcessResult
    private let savedProfile: HolonProcessResult
    private var recordedInvocations: [[String]] = []
    private var historyExists = false

    init(defaultsSnapshot: HolonProcessResult, savedProfile: HolonProcessResult) {
        self.defaultsSnapshot = defaultsSnapshot
        self.savedProfile = savedProfile
    }

    func run(executableURL: URL, arguments: [String]) async throws -> HolonProcessResult {
        recordedInvocations.append(arguments)
        if arguments == ["daemon", "status"], !historyExists {
            // Another operation saves desktop=false after this snapshot is read.
            historyExists = true
            return defaultsSnapshot
        }
        return savedProfile
    }

    func invocations() -> [[String]] {
        recordedInvocations
    }
}
