import Foundation
import XCTest
@testable import HolonMenu

final class HolonMenuPairingTests: XCTestCase {
    private final class AuthMethodURLProtocol: URLProtocol {
        static let lock = NSLock()
        nonisolated(unsafe) static var requests: [URLRequest] = []

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
        override func startLoading() {
            Self.lock.withLock { Self.requests.append(request) }
            let isAuthMethod = request.url?.path == "/api/auth/method"
            client?.urlProtocol(self, didReceive: HTTPURLResponse(
                url: request.url!, statusCode: isAuthMethod ? 200 : 401,
                httpVersion: nil, headerFields: nil
            )!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(#"{"mode":"local"}"#.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }
        override func stopLoading() {}
    }

    private func curlArguments(socket: String) -> [String] {
        ["--silent", "--show-error", "--fail", "--unix-socket", socket,
         "--request", "POST", "http://localhost/api/auth/pairing/issue"]
    }

    private func pairingClient(
        home: URL,
        options: HolonDaemonLaunchOptions = HolonDaemonLaunchOptions(),
        result: HolonProcessResult = HolonProcessResult(
            terminationStatus: 0, stdout: Data(#"{"ticket":"one-time-code"}"#.utf8),
            stderr: Data()
        )
    ) -> (HolonCLIClient, RecordingProcessLauncher) {
        AuthMethodURLProtocol.lock.withLock { AuthMethodURLProtocol.requests = [] }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [AuthMethodURLProtocol.self]
        let socket = home.appendingPathComponent("control.sock").path
        let status = """
            {"ok":true,"state":"running","healthy":true,"home_dir":"\(home.path)",
             "socket_path":"\(socket)","http_addr":"127.0.0.1:7878",
             "web_url":"http://127.0.0.1:7878","desired_running":true,
             "control_connectivity":true,"message":"Running"}
            """
        let launcher = RecordingProcessLauncher(
            result: .success(result),
            responses: ["daemon status": status]
        )
        return (HolonCLIClient(
            executableURL: URL(fileURLWithPath: "/opt/holon"),
            launcher: launcher, launchOptions: options,
            networkSession: URLSession(configuration: configuration)
        ), launcher)
    }

    func testPairingUsesUnixRegardlessOfTokenFileOrConfiguredCredential() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let tokenFile = home.appendingPathComponent("control.token")
        let socket = home.appendingPathComponent("control.sock").path

        // Missing, stale, insecure, and unreadable files must not choose TCP or block Unix.
        for fileState in ["missing", "stale", "insecure", "directory"] {
            if FileManager.default.fileExists(atPath: tokenFile.path) {
                try FileManager.default.removeItem(at: tokenFile)
            }
            if fileState == "directory" {
                try FileManager.default.createDirectory(at: tokenFile, withIntermediateDirectories: false)
            } else if fileState != "missing" {
                try Data("long-lived-secret".utf8).write(to: tokenFile)
                try FileManager.default.setAttributes(
                    [.posixPermissions: fileState == "insecure" ? 0o644 : 0o600],
                    ofItemAtPath: tokenFile.path
                )
            }
            let options = [
                HolonDaemonLaunchOptions(),
                HolonDaemonLaunchOptions(token: "configured-secret"),
                HolonDaemonLaunchOptions(tokenFilePath: tokenFile.path),
            ]
            for option in options {
                let (client, launcher) = pairingClient(home: home, options: option)
                let url = try await client.pairingURL(
                    for: URL(string: "https://holon.example.ts.net/old?discard=1#discard")!
                )
                XCTAssertEqual(
                    url.absoluteString, "https://holon.example.ts.net/login#pair=one-time-code",
                    fileState
                )
                let invocations = await launcher.invocations()
                XCTAssertEqual(invocations.count, 2)
                XCTAssertEqual(invocations.first?.arguments, ["daemon", "status"])
                XCTAssertEqual(invocations.last?.executableURL.path, "/usr/bin/curl")
                XCTAssertEqual(invocations.last?.arguments, curlArguments(socket: socket))
                let requests = AuthMethodURLProtocol.lock.withLock { AuthMethodURLProtocol.requests }
                XCTAssertEqual(requests.map { $0.url?.path }, ["/api/auth/method"])
                XCTAssertTrue(requests.allSatisfy {
                    $0.value(forHTTPHeaderField: "Authorization") == nil
                })
            }
        }
    }

    func testUnixIssuanceFailureDoesNotFallBackToTCPOrRestartDaemon() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let (client, launcher) = pairingClient(
            home: home, options: HolonDaemonLaunchOptions(token: "configured-secret"),
            result: HolonProcessResult(
                terminationStatus: 22, stdout: Data(), stderr: Data("HTTP 401".utf8)
            )
        )
        do {
            _ = try await client.pairingURL(for: URL(string: "https://holon.example.ts.net")!)
            XCTFail("Unix issuance failure must not succeed via TCP")
        } catch HolonCLIError.pairingFailed {
            // Expected: local issuance fails closed.
        }
        let invocations = await launcher.invocations()
        XCTAssertEqual(invocations.count, 2)
        XCTAssertEqual(invocations.last?.arguments, curlArguments(
            socket: home.appendingPathComponent("control.sock").path
        ))
        let requests = AuthMethodURLProtocol.lock.withLock { AuthMethodURLProtocol.requests }
        XCTAssertEqual(requests.map { $0.url?.path }, ["/api/auth/method"])
    }

    func testUnixIssuanceRejectsMalformedOrEmptyTicket() async throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        for body in ["not-json", #"{"ticket":""}"#] {
            let (client, _) = pairingClient(
                home: home,
                result: HolonProcessResult(
                    terminationStatus: 0, stdout: Data(body.utf8), stderr: Data()
                )
            )
            do {
                _ = try await client.authenticatedWebURL()
                XCTFail("Invalid ticket must not produce a pairing URL")
            } catch is DecodingError {
                // Malformed response.
            } catch HolonCLIError.pairingFailed {
                // Empty ticket.
            }
        }
    }
}
