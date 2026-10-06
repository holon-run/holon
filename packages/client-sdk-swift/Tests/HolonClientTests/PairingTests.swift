import Foundation
import HolonClient
import XCTest

@MainActor
final class PairingTests: XCTestCase {
    func testOfflineInvitationRequiresHTTPConfirmation() throws {
        let ticket = String(repeating: "a", count: 64)
        let invitation = try HolonPairingInvitation(
            payload: "http://100.64.0.1:7878/login#pair=\(ticket)")
        XCTAssertEqual(invitation.apiBaseURL.absoluteString, "http://100.64.0.1:7878/api/")
        XCTAssertEqual(invitation.ticket, ticket)
        XCTAssertNil(invitation.expiresAt)
        XCTAssertThrowsError(try invitation.endpoint())
        XCTAssertNoThrow(try invitation.endpoint(allowInsecureHTTP: true))
    }

    func testOfflineInvitationRejectsMalformedPayloads() {
        let ticket = String(repeating: "a", count: 64)
        for payload in [
            "https://example.test/login?ticket=x#pair=\(ticket)",
            "https://user@example.test/login#pair=\(ticket)",
            "https://example.test/other#pair=\(ticket)",
            "https://example.test/login#pair=short",
            "https://example.test/login#pair=\(ticket)&extra=1",
            "ftp://example.test/login#pair=\(ticket)",
            "https://example.test/login#pair=%61\(ticket.dropFirst())"
        ] {
            XCTAssertThrowsError(try HolonPairingInvitation(payload: payload), payload)
        }
    }

    func testPrefixTraversalLoopbackConfirmationAndRedactedDescriptions() throws {
        let ticket = String(repeating: "a", count: 64)
        let prefixed = try HolonPairingInvitation(payload: "https://example.test/proxy/login#pair=\(ticket)")
        XCTAssertEqual(prefixed.apiBaseURL.absoluteString, "https://example.test/proxy/api/")
        XCTAssertFalse(String(describing: prefixed).contains(ticket))
        XCTAssertFalse(String(reflecting: prefixed).contains(ticket))
        for path in ["/../login", "/%2e%2e/login", "/a%2fb/login"] {
            XCTAssertThrowsError(try HolonPairingInvitation(payload: "https://example.test\(path)#pair=\(ticket)"))
        }
        let loopback = try HolonPairingInvitation(payload: "http://127.0.0.1/login#pair=\(ticket)")
        XCTAssertThrowsError(try loopback.endpoint())
        XCTAssertNoThrow(try loopback.endpoint(allowInsecureHTTP: true))
        XCTAssertThrowsError(try HolonPairingInvitation(payload: String(repeating: "a", count: 2049)))
    }

    private func client(_ replies: [MockReply]) throws -> (HolonClient, MockExchange) {
        let host = "\(UUID().uuidString.lowercased()).test"
        let exchange = MockExchange(replies)
        MockHTTP.register(exchange, host: host)
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockHTTP.self]
        let endpoint = try HolonEndpoint(apiBaseURL: URL(string: "https://\(host)/prefix/api")!)
        let sdk = try HolonClient(endpoint: endpoint, networkID: "pairing",
                                  credential: "existing-session", configuration: config)
        addTeardownBlock {
            await sdk.close()
            MockHTTP.unregister(host: host)
        }
        return (sdk, exchange)
    }

    func testNativeRedemptionIsUnauthenticatedAndPreservesPrefix() async throws {
        let body = Data(#"{"ok":true,"credential":"new-session","user_id":"u","expires_at":null}"#.utf8)
        let (sdk, exchange) = try client([MockReply(body: body)])
        let ticket = String(repeating: "aF", count: 32)
        let response = try await sdk.redeemPairingTicket(ticket: ticket)
        XCTAssertEqual(response.value.credential, "new-session")
        let request = try XCTUnwrap(exchange.requests.first)
        XCTAssertEqual(request.url?.path, "/prefix/api/auth/pairing/redeem/native")
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
        let data = try XCTUnwrap(request.httpBody)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: String])
        XCTAssertEqual(json, ["ticket": ticket])
        XCTAssertEqual(exchange.requests.count, 1)
    }

    func testInvalidTicketsNeverReachTransport() async throws {
        let (sdk, exchange) = try client([])
        for ticket in ["", String(repeating: "a", count: 63), String(repeating: "a", count: 65),
                       String(repeating: "g", count: 64), String(repeating: "é", count: 64)] {
            do {
                _ = try await sdk.redeemPairingTicket(ticket: ticket)
                XCTFail("Expected rejection")
            } catch { XCTAssertEqual(error as? HolonClientError, .invalidRequest) }
        }
        XCTAssertTrue(exchange.requests.isEmpty)
    }

    func testRedemptionNeverRetriesAndRejectsMalformedSessions() async throws {
        let ticket = String(repeating: "0", count: 64)
        let (sdk, exchange) = try client([MockReply(status: 503, body: Data("{}".utf8))])
        do { _ = try await sdk.redeemPairingTicket(ticket: ticket); XCTFail() }
        catch { XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 503) }
        XCTAssertEqual(exchange.requests.count, 1)
        for body in [#"{"ok":false,"user_id":"u","credential":"session"}"#,
                     #"{"ok":true,"user_id":"u","credential":" "}"#, #"{"ok":true,"user_id":"u"}"#] {
            let (invalid, _) = try client([MockReply(body: Data(body.utf8))])
            do { _ = try await invalid.redeemPairingTicket(ticket: ticket); XCTFail() }
            catch { XCTAssertEqual(error as? HolonClientError, .malformedResponse) }
        }
    }
}

private struct MockReply: Sendable {
    var status = 200
    var body = Data()
    var contentType = "application/json"
    var headers: [String: String] = [:]
    var delay: TimeInterval = 0
    var error: URLError.Code? = nil
}

private final class MockExchange: @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [MockReply]
    private var captured: [URLRequest] = []
    init(_ replies: [MockReply]) { self.replies = replies }
    var requests: [URLRequest] { lock.withLock { captured } }
    func reply(for request: URLRequest) -> MockReply {
        var request = request
        if request.httpBody == nil, let stream = request.httpBodyStream {
            stream.open()
            defer { stream.close() }
            var body = Data()
            var buffer = [UInt8](repeating: 0, count: 1024)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                guard count > 0 else { break }
                body.append(contentsOf: buffer.prefix(count))
            }
            request.httpBody = body
        }
        return lock.withLock {
            captured.append(request)
            return replies.isEmpty ? MockReply(status: 500) : replies.removeFirst()
        }
    }
}

private final class MockHTTP: URLProtocol, @unchecked Sendable {
    private static let registryLock = NSLock()
    nonisolated(unsafe) private static var registry: [String: MockExchange] = [:]
    private let lock = NSLock()
    private var stopped = false
    static func register(_ exchange: MockExchange, host: String) {
        registryLock.withLock { registry[host] = exchange }
    }
    static func unregister(host: String) { _ = registryLock.withLock { registry.removeValue(forKey: host) } }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let exchange = Self.registryLock.withLock { Self.registry[request.url!.host!]! }
        let reply = exchange.reply(for: request)
        DispatchQueue.global().asyncAfter(deadline: .now() + reply.delay) { [self] in
            lock.withLock {
                guard !stopped else { return }
                if let error = reply.error { client?.urlProtocol(self, didFailWithError: URLError(error)); return }
                var headers = reply.headers
                headers["Content-Type"] = reply.contentType
                let response = HTTPURLResponse(url: request.url!, statusCode: reply.status,
                                               httpVersion: "HTTP/1.1", headerFields: headers)!
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: reply.body)
                client?.urlProtocolDidFinishLoading(self)
            }
        }
    }
    override func stopLoading() { lock.withLock { stopped = true } }
}
