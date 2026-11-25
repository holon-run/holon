import Foundation
import HolonClient
import XCTest
@testable import Holon

private final class LegacyModelProtocol: URLProtocol, @unchecked Sendable {
    private final class Fixture: @unchecked Sendable {
        let lock = NSLock()
        var waiting: LegacyModelProtocol?
        var arrived: XCTestExpectation?
        var promptCount = 0
    }
    private static let fixture = Fixture()

    static func install(arrived: XCTestExpectation) {
        fixture.lock.withLock {
            fixture.arrived = arrived
            fixture.promptCount = 0
        }
    }

    static var promptCount: Int { fixture.lock.withLock { fixture.promptCount } }

    @discardableResult
    static func finishWrite() -> Bool {
        let pending = fixture.lock.withLock {
            let pending = fixture.waiting
            fixture.waiting = nil
            fixture.arrived = nil
            return pending
        }
        pending?.reply(status: 200, body: #"{"ok":true,"model":{"effective_model":"test/legacy"}}"#)
        return pending != nil
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        if request.httpMethod == "POST", request.url?.path.hasSuffix("/control/agents/A/model") == true {
            Self.fixture.lock.withLock {
                Self.fixture.waiting = self
                Self.fixture.arrived?.fulfill()
            }
        } else {
            Self.fixture.lock.withLock { Self.fixture.promptCount += 1 }
            reply(status: 500, body: "{}")
        }
    }

    override func stopLoading() {
        Self.fixture.lock.withLock {
            if Self.fixture.waiting === self { Self.fixture.waiting = nil }
        }
    }

    private func reply(status: Int, body: String) {
        guard let url = request.url,
              let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: nil,
                                             headerFields: ["Content-Type": "application/json"]) else { return }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
}

final class SendingTransportTests: XCTestCase {
    @MainActor
    func testLocalCancellationWaitsForLegacyModelHTTPResponseWithoutPostingPrompt() async throws {
        let base = URL(string: "https://sending.invalid/api")!
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [LegacyModelProtocol.self]
        let client = try HolonClient(endpoint: HolonEndpoint(apiBaseURL: base),
                                    networkID: "network", configuration: configuration)
        try await client.bindIdentity(runtimeID: "runtime", userID: "user", visibilityScopeID: "private", credential: nil)
        let suite = "SendingTransportTests.\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let consent = SharingConsent(defaults: defaults)
        consent.approve(base)
        let transport = SendingClientTransport(client: client, authority: await client.identity,
                                               apiBaseURL: base, consent: consent)
        let arrived = XCTestExpectation(description: "model POST reached HTTP transport")
        let endedEarly = XCTestExpectation(description: "delivery ended while model POST awaited its response")
        endedEarly.isInverted = true
        LegacyModelProtocol.install(arrived: arrived)
        let delivery = Task {
            try await transport.send(agentID: "A", requestID: UUID(),
                                     payload: SendingPayload(text: "draft", modelID: "test/legacy", attachments: []))
        }
        var responseReleased = false
        let observe = Task {
            _ = await delivery.result
            if !responseReleased { endedEarly.fulfill() }
        }
        let arrival = await XCTWaiter.fulfillment(of: [arrived], timeout: 3)
        XCTAssertEqual(arrival, .completed)
        delivery.cancel()
        let earlyEnd = await XCTWaiter.fulfillment(of: [endedEarly], timeout: 0.3)
        XCTAssertEqual(earlyEnd, .completed)
        responseReleased = true
        let finished = LegacyModelProtocol.finishWrite()
        XCTAssertTrue(finished, "URLSession must still be awaiting the model POST response")
        if !finished { await client.close() }
        do {
            _ = try await delivery.value
            XCTFail("The cancelled delivery must not continue to the prompt POST")
        } catch is CancellationError {}
        await observe.value
        XCTAssertEqual(LegacyModelProtocol.promptCount, 0)
        await client.close()
    }
}
