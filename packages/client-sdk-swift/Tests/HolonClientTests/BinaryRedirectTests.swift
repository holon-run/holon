import Foundation
import HolonClient
import Network
import XCTest

@MainActor
final class BinaryRedirectTests: XCTestCase {
    func testRealCrossOriginRedirectNeverSendsCredentialToDestination() async throws {
        let destinationReady = expectation(description: "Destination listening")
        let destination = try BinaryHTTPFixture(response: "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
                                               ready: destinationReady)
        defer { destination.stop() }
        await fulfillment(of: [destinationReady], timeout: 3)
        let destinationPort = try XCTUnwrap(destination.port)
        let sourceReady = expectation(description: "Source listening")
        let source = try BinaryHTTPFixture(response: "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:\(destinationPort)/sink\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                          ready: sourceReady)
        defer { source.stop() }
        await fulfillment(of: [sourceReady], timeout: 3)
        let sourcePort = try XCTUnwrap(source.port)
        let sdk = try HolonClient(endpoint: HolonEndpoint(apiBaseURL:
            URL(string: "http://127.0.0.1:\(sourcePort)/prefix/api")!),
            networkID: "redirect-test", credential: "test-only-secret")
        do {
            _ = try await sdk.downloadWorkspaceFile(workspaceID: "ws", path: "a")
            XCTFail("Redirect must never return destination bytes")
        } catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 302) }
        await sdk.close()
        XCTAssertEqual(source.requests.count, 1)
        XCTAssertTrue(source.requests[0].lowercased().contains("authorization: bearer test-only-secret"))
        XCTAssertTrue(source.requests[0].contains("/prefix/api/workspaces/ws/files/a?download=true"))
        XCTAssertEqual(destination.requests.count, 0, "No request or credential may reach the second origin")
    }
}

/// Loopback-only HTTP fixture exercises URLSession's real redirect delegate.
private final class BinaryHTTPFixture: @unchecked Sendable {
    private let listener: NWListener
    private let lock = NSLock()
    private var captured: [String] = []
    private var connections: [NWConnection] = []
    var port: UInt16? { listener.port?.rawValue }
    var requests: [String] { lock.withLock { captured } }

    init(response: String, ready: XCTestExpectation) throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
        listener.stateUpdateHandler = { state in
            if case .ready = state { ready.fulfill() }
        }
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { connection.cancel(); return }
            lock.withLock { connections.append(connection) }
            connection.start(queue: .global())
            connection.receive(minimumIncompleteLength: 1, maximumLength: 65_536) { [weak self] data, _, _, _ in
                if let data {
                    self?.lock.withLock { self?.captured.append(String(decoding: data, as: UTF8.self)) }
                    connection.send(content: Data(response.utf8), completion: .contentProcessed { _ in
                        connection.cancel()
                    })
                } else { connection.cancel() }
            }
        }
        listener.start(queue: .global())
    }

    func stop() {
        listener.cancel()
        lock.withLock { connections.forEach { $0.cancel() }; connections.removeAll() }
    }
}
