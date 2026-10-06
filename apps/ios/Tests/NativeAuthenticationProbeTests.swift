import AuthenticationServices
import UIKit
import XCTest
@testable import Holon

@MainActor
final class NativeAuthenticationProbeTests: XCTestCase {
    private let base = URL(string: "https://holon.example/proxy/api/")!

    func testS256VectorAndPrefixedStartURL() throws {
        let proof = NativeLoginProof(
            apiBaseURL: base, state: String(repeating: "s", count: 43),
            verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", createdAt: Date()
        )
        XCTAssertEqual(proof.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        let url = try proof.startURL()
        XCTAssertEqual(url.path, "/proxy/api/auth/oidc/native/start")
        let items = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems)
        XCTAssertEqual(items.first { $0.name == "client" }?.value, "ios")
        XCTAssertEqual(items.first { $0.name == "state" }?.value, proof.state)
        XCTAssertEqual(items.first { $0.name == "code_challenge" }?.value, proof.challenge)
        XCTAssertEqual(items.first { $0.name == "code_challenge_method" }?.value, "S256")
        XCTAssertThrowsError(try NativeLoginProof.make(apiBaseURL: URL(string: "http://holon.example/api/")!))
        XCTAssertThrowsError(try NativeLoginProof.make(apiBaseURL: URL(string: "https://user:secret@holon.example/api/")!))
    }

    func testCallbackBindsProofStateAndFixedEndpoint() throws {
        let proof = try NativeLoginProof.make(apiBaseURL: base)
        let ticket = String(repeating: "a", count: 64)
        let callback = "run.holon.ios://oidc/callback?state=\(proof.state)&ticket=\(ticket)&code_challenge_method=S256"
        XCTAssertEqual(try proof.ticket(from: URL(string: callback)!), ticket)
        let invalid = [
            callback.replacingOccurrences(of: "run.holon.ios", with: "run.holon.android"),
            callback.replacingOccurrences(of: "/callback", with: "/other"),
            callback.replacingOccurrences(of: proof.state, with: "wrong"),
            callback + "&state=\(proof.state)",
            callback + "&ticket=\(ticket)",
            callback.replacingOccurrences(of: "S256", with: "plain"),
            callback + "#fragment",
        ]
        for value in invalid {
            XCTAssertThrowsError(try proof.ticket(from: URL(string: value)!))
        }
        XCTAssertThrowsError(try proof.ticket(
            from: URL(string: callback)!, now: proof.createdAt.addingTimeInterval(NativeLoginProof.lifetime)
        ))
    }

    func testKeychainProofReopenScopeExpirationAndRemoval() throws {
        let vault = CredentialVault(service: "run.holon.ios.proof-tests.\(UUID().uuidString)")
        let store = NativeLoginProofStore(vault: vault)
        let proof = try NativeLoginProof.make(apiBaseURL: base)
        defer { try? store.remove(proof) }
        try store.save(proof)
        let reopened = NativeLoginProofStore(vault: CredentialVault(service: vault.service))
        XCTAssertEqual(try reopened.load(apiBaseURL: base, state: proof.state), proof)
        XCTAssertEqual(try reopened.loadPending(apiBaseURL: base), proof)
        XCTAssertNil(try reopened.load(apiBaseURL: URL(string: "https://holon.example/other/api/")!, state: proof.state))
        XCTAssertNil(try reopened.load(apiBaseURL: base, state: "other-state"))
        try store.remove(proof)
        XCTAssertNil(try reopened.load(apiBaseURL: base, state: proof.state))
        XCTAssertNil(try reopened.loadPending(apiBaseURL: base))
        try store.save(proof)
        XCTAssertNil(try reopened.load(
            apiBaseURL: base, state: proof.state, now: proof.createdAt.addingTimeInterval(NativeLoginProof.lifetime)
        ))
        XCTAssertNil(try reopened.load(apiBaseURL: base, state: proof.state))
    }

    func testProofReplacementExpiryAndDescriptionsDoNotExposeSecrets() throws {
        let store = NativeLoginProofStore(vault: .init(service: "run.holon.ios.proof-tests.\(UUID())"))
        let first = try NativeLoginProof.make(apiBaseURL: base)
        let second = try NativeLoginProof.make(apiBaseURL: base)
        defer { try? store.remove(first); try? store.remove(second) }
        try store.save(first)
        try store.save(second)
        XCTAssertNil(try store.load(apiBaseURL: base, state: first.state))
        try store.remove(first)
        XCTAssertEqual(try store.loadPending(apiBaseURL: base), second)
        XCTAssertFalse(String(describing: second).contains(second.verifier))
        XCTAssertFalse(String(reflecting: second).contains(second.state))
        XCTAssertNil(try store.loadPending(apiBaseURL: base,
                                           now: second.createdAt.addingTimeInterval(NativeLoginProof.lifetime)))
        XCTAssertNil(try store.load(apiBaseURL: base, state: second.state))
    }

    func testBrowserPreparationAnchorEphemeralPolicyAndCancellation() throws {
        let anchor = UIWindow()
        let browser = NativeBrowserAuthentication(anchor: anchor)
        let proof = try NativeLoginProof.make(apiBaseURL: base)
        let cancelled = expectation(description: "single cancellation")
        cancelled.assertForOverFulfill = true
        try browser.prepare(proof: proof) { result in
            guard case .failure(let error) = result,
                  case NativeBrowserAuthentication.Failure.cancelled = error else {
                XCTFail("Expected explicit browser cancellation")
                return
            }
            cancelled.fulfill()
        }
        let session = try XCTUnwrap(browser.session)
        let types = Bundle.main.object(forInfoDictionaryKey: "CFBundleURLTypes") as? [[String: Any]]
        let schemes = types?.flatMap { $0["CFBundleURLSchemes"] as? [String] ?? [] } ?? []
        XCTAssertTrue(schemes.contains(NativeLoginProof.callbackScheme))
        XCTAssertFalse(schemes.contains("run.holon.android"))
        XCTAssertTrue(session.prefersEphemeralWebBrowserSession)
        XCTAssertTrue(session.presentationContextProvider === browser)
        XCTAssertTrue(browser.presentationAnchor(for: session) === anchor)
        XCTAssertThrowsError(try browser.prepare(proof: proof) { _ in })
        browser.cancel()
        browser.cancel()
        XCTAssertNil(browser.session)
        XCTAssertFalse(browser.start())
        wait(for: [cancelled], timeout: 1)
    }
}
