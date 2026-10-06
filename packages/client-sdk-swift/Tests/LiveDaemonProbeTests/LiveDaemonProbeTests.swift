import Foundation
import HolonClient
import HolonWire
import XCTest
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

// These are transport probes, not a production SDK or an automatic retry policy.
final class LiveDaemonProbeTests: XCTestCase {
    private func base(_ key: String = "HOLON_LIVE_BASE") throws -> URL {
        guard let value = ProcessInfo.processInfo.environment[key],
              let url = URL(string: value) else {
            throw XCTSkip("Run make ios-contract-test to start the isolated daemon")
        }
        return url
    }

    func testProductionSDKBootstrapThroughRealPrefix() async throws {
        for key in ["HOLON_LIVE_BASE", "HOLON_LIVE_PROXY_BASE"] {
            let endpoint = try HolonEndpoint(apiBaseURL: base(key))
            let client = try HolonClient(endpoint: endpoint, networkID: key)
            do {
                let handshake = try await client.handshake()
                guard case .compatible(let server) =
                        handshake.value.checkCompatibility(requiredCapabilities: ["agents.list"]) else {
                    return XCTFail("Current daemon must be compatible with the production SDK")
                }
                XCTAssertEqual(server.defaultAgentId, "main")
                let roster = try await client.listAgents()
                XCTAssertEqual(roster.identity, handshake.identity)
                XCTAssertEqual(roster.value.agents.map(\.id), ["main"])
                await client.close()
            } catch { await client.close(); throw error }
        }
    }

    func testProductionSDKStreamEOFAndExplicitReopen() async throws {
        let endpoint = try HolonEndpoint(apiBaseURL: base("HOLON_LIVE_PROXY_BASE"))
        let client = try HolonClient(endpoint: endpoint, networkID: "isolated-proxy")
        do {
            for _ in 0..<2 {
                let connection = try await client.openEventStream(path: ["events", "stream"])
                var events = connection.makeAsyncIterator()
                do { _ = try await events.next(); XCTFail("A closed stream must request recovery") }
                catch { XCTAssertEqual(error as? HolonClientError, .streamEnded) }
                connection.close()
                _ = try await client.handshake()
            }
            await client.close()
        } catch { await client.close(); throw error }
    }

    func testProductionSDKStreamCloseAndIdentityCancellation() async throws {
        let endpoint = try HolonEndpoint(apiBaseURL: base())
        let client = try HolonClient(endpoint: endpoint, networkID: "isolated")
        do {
            let connection = try await client.openEventStream(path: ["events", "stream"])
            connection.close()
            var events = connection.makeAsyncIterator()
            do { _ = try await events.next(); XCTFail("Explicit close must end iteration") }
            catch { XCTAssertTrue(error is CancellationError || (error as? URLError)?.code == .cancelled) }
            let previous = await client.identity
            let next = try await client.bindIdentity(runtimeID: "new-runtime", userID: nil,
                                                      visibilityScopeID: nil, credential: nil)
            XCTAssertNotEqual(previous, next)
            _ = try await client.handshake()
            await client.close()
        } catch { await client.close(); throw error }
    }

    private func bootstrap(_ base: URL) async throws {
        let session = URLSession(configuration: .ephemeral)
        defer { session.invalidateAndCancel() }
        for path in ["handshake", "agents/list"] {
            let (data, response) = try await session.data(from: base.appendingPathComponent(path))
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
            if path == "handshake" {
                let handshake = try JSONDecoder().decode(HandshakeResponse.self, from: data)
                XCTAssertTrue(handshake.ok)
                XCTAssertEqual(handshake._protocol.name, "holon-control")
                XCTAssertEqual(handshake._protocol.version, 1)
            } else {
                let agents = try JSONDecoder().decode([AgentListEntry].self, from: data)
                XCTAssertEqual(agents.map(\.identity.agentId), ["main"],
                               "Only the isolated daemon's fresh default agent may exist")
            }
        }
    }

    func testHTTPBootstrapAndRealPrefixProxy() async throws {
        try await bootstrap(base())
        try await bootstrap(base("HOLON_LIVE_PROXY_BASE"))
    }

    func testHTTPSCannotSilentlyFallBackToPlainHTTP() async throws {
        let http = try base()
        var components = URLComponents(url: http, resolvingAgainstBaseURL: false)!
        components.scheme = "https"
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 3
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        do {
            _ = try await session.data(from: components.url!.appendingPathComponent("handshake"))
            XCTFail("A plaintext daemon must not be accepted as TLS")
        } catch {
            XCTAssertTrue(error is URLError, "Expected a transport/TLS failure: \(error)")
        }
    }

    func testSSECloseRebootstrapAndCancellation() async throws {
        let direct = try base()
        let proxy = try base("HOLON_LIVE_PROXY_BASE")
        // The real proxy connects upstream, forwards SSE headers, then closes
        // this downstream stream deliberately. No mock event is emitted.
        let closed = expectation(description: "Proxy closes the real SSE connection")
        let connected = expectation(description: "Real SSE response")
        let delegate = StreamObserver(connected: connected, completed: closed)
        let session = URLSession(configuration: .ephemeral, delegate: delegate, delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        session.dataTask(with: proxy.appendingPathComponent("events/stream")).resume()
        await fulfillment(of: [connected, closed], timeout: 5)
        try await bootstrap(direct)

        let reconnected = expectation(description: "Reconnect after fresh bootstrap")
        let cancelled = expectation(description: "Explicit cancellation completes")
        let cancellationDelegate = StreamObserver(
            connected: reconnected, completed: cancelled, expectsCancellation: true)
        let cancellationSession = URLSession(
            configuration: .ephemeral, delegate: cancellationDelegate, delegateQueue: nil)
        defer { cancellationSession.invalidateAndCancel() }
        let task = cancellationSession.dataTask(with: direct.appendingPathComponent("events/stream"))
        task.resume()
        // Foundation may buffer headers until the daemon's 15-second heartbeat.
        await fulfillment(of: [reconnected], timeout: 20)
        task.cancel()
        await fulfillment(of: [cancelled], timeout: 5)
        try await bootstrap(direct)
    }
}

private final class StreamObserver: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    let connected: XCTestExpectation
    let completed: XCTestExpectation
    let expectsCancellation: Bool

    init(connected: XCTestExpectation, completed: XCTestExpectation,
         expectsCancellation: Bool = false) {
        self.connected = connected
        self.completed = completed
        self.expectsCancellation = expectsCancellation
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask,
                    didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        let http = response as? HTTPURLResponse
        XCTAssertEqual(http?.statusCode, 200)
        XCTAssertTrue(http?.value(forHTTPHeaderField: "Content-Type")?
            .hasPrefix("text/event-stream") == true)
        connected.fulfill()
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    didCompleteWithError error: (any Error)?) {
        if expectsCancellation {
            XCTAssertEqual((error as? URLError)?.code, .cancelled)
        } else {
            XCTAssertNil(error)
        }
        completed.fulfill()
    }
}
