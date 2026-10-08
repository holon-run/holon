import Foundation
import HolonClient
import Network
import XCTest

@MainActor
final class BinaryRedirectTests: XCTestCase {
    func testBinaryProgressIsBoundedMonotoneAndReportsFinalOriginalBytes() async throws {
        let ready = expectation(description: "Progress source listening")
        let source = try BinaryHTTPFixture(response: "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 131073\r\nConnection: close\r\n\r\n" + String(repeating: "x", count: 131_073), ready: ready)
        defer { source.stop() }; await fulfillment(of: [ready], timeout: 3)
        let port = try XCTUnwrap(source.port), progress = BinaryProgressCapture()
        let client = try HolonClient(endpoint: HolonEndpoint(apiBaseURL: URL(string: "http://127.0.0.1:\(port)/api")!), networkID: "progress")
        let reply = try await client.downloadWorkspaceFile(workspaceID: "w", path: "file.txt", progress: { progress.add($0) })
        XCTAssertEqual(reply.value.data.count, 131_073)
        let values = progress.values
        XCTAssertEqual(values.first?.receivedBytes, 0); XCTAssertEqual(values.last?.receivedBytes, 131_073)
        XCTAssertTrue(values.allSatisfy { $0.totalBytes == 131_073 && $0.receivedBytes <= 131_073 })
        XCTAssertEqual(values.map(\.receivedBytes), values.map(\.receivedBytes).sorted())
        XCTAssertLessThanOrEqual(values.count, 5)
        await client.close()
    }
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

private final class BinaryProgressCapture: @unchecked Sendable {
    private let lock = NSLock()
    private var captured: [HolonDownloadProgress] = []
    var values: [HolonDownloadProgress] { lock.withLock { captured } }
    func add(_ value: HolonDownloadProgress) { lock.withLock { captured.append(value) } }
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
