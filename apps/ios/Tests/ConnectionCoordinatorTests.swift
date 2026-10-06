import Foundation
import HolonClient
import XCTest
@testable import Holon

/// close deliberately does not release or cancel requests: the coordinator must fence results.
private actor CoordinatorTransport: ConnectionTransport {
    enum Gate { case user, exchange }
    enum BootstrapFailure { case authenticationMode, incompatibleHandshake, forbiddenUser, offlineUser }
    let mode: String
    var userID = "user"
    var runtimeID = "runtime"
    var visibility = "private"
    var userFailure: (any Error)?
    var exchangeFailure: (any Error)?
    var revokeFailure = false
    var gate: Gate?
    var blocked: CheckedContinuation<Void, Never>?
    var entered = false
    var observers: [CheckedContinuation<Void, Never>] = []
    var exchanges: [(String, String?)] = []
    var redemptions: [String] = []
    var bindings: [StoredSession?] = []
    var handshakeCredentials: [String?] = []
    var issuedUserID: String?
    var closes = 0
    var bootstrapFailure: BootstrapFailure?

    init(mode: String = "local", gate: Gate? = nil) {
        self.mode = mode
        self.gate = gate
    }
    func waitUntilEntered() async {
        if entered { return }
        await withCheckedContinuation { observers.append($0) }
    }
    private func pause(_ point: Gate) async {
        guard gate == point else { return }
        await withCheckedContinuation { continuation in
            blocked = continuation
            entered = true
            observers.forEach { $0.resume() }
            observers.removeAll()
        }
    }
    func release() {
        gate = nil
        blocked?.resume()
        blocked = nil
    }
    func block(_ point: Gate) {
        gate = point
        entered = false
    }
    func configure(user: String = "user", runtime: String = "runtime", visibility: String = "private") {
        userID = user
        runtimeID = runtime
        self.visibility = visibility
    }
    func failUser(_ error: any Error) { userFailure = error }
    func failExchange(_ error: any Error) { exchangeFailure = error }
    func failRevocation() { revokeFailure = true }
    func setIssuedUser(_ user: String) { issuedUserID = user }
    func failBootstrap(_ failure: BootstrapFailure) { bootstrapFailure = failure }
    func authenticationMode() async throws -> String {
        if bootstrapFailure == .authenticationMode { throw URLError(.notConnectedToInternet) }
        return mode
    }
    func handshake() async throws -> HolonHandshake {
        let credential = bindings.last.flatMap { $0?.credential }
        handshakeCredentials.append(credential)
        guard credential != nil else {
            throw HolonHTTPFailure(statusCode: 401, identity: .init(networkID: "fixture"))
        }
        return try HolonHandshake(data: Data("""
        {"ok":true,"protocol":{"name":"holon-control","version":\(bootstrapFailure == .incompatibleHandshake ? 999 : 1)},
         "auth":{"mode":"\(mode == "oidc" ? "bearer" : "local")","required":true},"capabilities":[],
         "runtime":{"default_agent":"main","home_dir":"/home","workspace_dir":"/work","listen":"127.0.0.1:8787"}}
        """.utf8))
    }
    func currentUser() async throws -> HolonCurrentUser {
        await pause(.user)
        if bootstrapFailure == .forbiddenUser {
            throw HolonHTTPFailure(statusCode: 403, identity: .init(networkID: "fixture"))
        }
        if bootstrapFailure == .offlineUser { throw URLError(.notConnectedToInternet) }
        if let userFailure { throw userFailure }
        return try HolonCurrentUser(data: Data("""
        {"ok":true,"user_id":"\(userID)","auth_method":"bearer"}
        """.utf8))
    }
    func scope() async throws -> (runtimeID: String, visibilityScopeID: String) {
        (runtimeID, visibility)
    }
    private func issuedSession() throws -> HolonSession {
        try HolonSession(data: Data("""
        {"ok":true,"credential":"issued-session","user_id":"\(issuedUserID ?? userID)","expires_at":null}
        """.utf8))
    }
    func exchange(_ credential: String, verifier: String?) async throws -> HolonSession {
        exchanges.append((credential, verifier))
        await pause(.exchange)
        if let exchangeFailure { throw exchangeFailure }
        return try issuedSession()
    }
    func redeem(_ ticket: String) async throws -> HolonSession {
        redemptions.append(ticket)
        return try issuedSession()
    }
    func bind(_ session: StoredSession?) async throws -> HolonConnectionIdentity {
        bindings.append(session)
        return HolonConnectionIdentity(networkID: "fixture", runtimeID: session?.runtimeID,
                                       userID: session?.userID, visibilityScopeID: session?.visibilityScopeID)
    }
    func revoke() async throws { if revokeFailure { throw URLError(.notConnectedToInternet) } }
    func close() async { closes += 1 }
}

@MainActor
final class ConnectionCoordinatorTests: XCTestCase {
    @MainActor private struct Fixture {
        let suite: String
        let defaults: UserDefaults
        let vault: CredentialVault
        let proofs: NativeLoginProofStore
        let store: ConnectionStore
        func reopen() throws -> ConnectionStore { try ConnectionStore(defaults: defaults, vault: vault) }
    }

    private func fixture() throws -> Fixture {
        let suite = "run.holon.ios.coordinator-tests.\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        let vault = CredentialVault(service: suite)
        let proofs = NativeLoginProofStore(vault: CredentialVault(service: suite + ".proofs"))
        let store = try ConnectionStore(defaults: defaults, vault: vault)
        addTeardownBlock { @MainActor in
            for profile in store.profiles {
                if let proof = try proofs.loadPending(apiBaseURL: profile.apiBaseURL) { try proofs.remove(proof) }
                try store.removeProfile(profile.id)
            }
            defaults.removePersistentDomain(forName: suite)
        }
        return Fixture(suite: suite, defaults: defaults, vault: vault, proofs: proofs, store: store)
    }
    private func profile(_ fixture: Fixture, name: String = "A") throws -> ConnectionProfile {
        let profile = try ConnectionProfile(name: name, apiBaseURL: XCTUnwrap(URL(string: "https://\(name.lowercased()).example.test/api")))
        try fixture.store.saveProfile(profile)
        return profile
    }
    private func saved(_ fixture: Fixture, profile: ConnectionProfile, credential: String = "saved-session") throws {
        try fixture.store.saveSession(StoredSession(credential: credential, runtimeID: "runtime", userID: "user",
                                                   visibilityScopeID: "private", expiresAt: nil), for: profile)
    }
    private func coordinator(_ fixture: Fixture, transport: CoordinatorTransport,
                             deadline: Duration = .seconds(30)) -> ConnectionCoordinator {
        ConnectionCoordinator(store: fixture.store, proofStore: fixture.proofs, deadline: deadline,
                              makeTransport: { _ in transport })
    }
    private func callback(_ proof: NativeLoginProof, state: String? = nil) throws -> URL {
        try XCTUnwrap(URL(string: "run.holon.ios://oidc/callback?state=\(state ?? proof.state)&ticket=\(String(repeating: "a", count: 64))&code_challenge_method=S256"))
    }

    func testSavedCredentialIsBoundBeforeHandshake() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        XCTAssertEqual(c.status, .connected)
        let credentials = await transport.handshakeCredentials
        XCTAssertEqual(credentials, ["saved-session"])
    }

    func testNewOIDCConnectionDoesNotAttemptUnauthenticatedHandshake() async throws {
        let f = try fixture()
        let p = try profile(f)
        let transport = CoordinatorTransport(mode: "oidc")
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        XCTAssertEqual(c.status, .needsLogin)
        let credentials = await transport.handshakeCredentials
        XCTAssertTrue(credentials.isEmpty)
    }

    func testLocalSessionUsesCurrentUserInsteadOfIssuedUser() async throws {
        let f = try fixture()
        let p = try profile(f)
        let transport = CoordinatorTransport()
        await transport.configure(user: "control")
        await transport.setIssuedUser("local-static-token")
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        await c.login(token: "bootstrap-token")
        XCTAssertEqual(c.status, .connected)
        XCTAssertEqual(try f.store.session(for: p)?.userID, "control")
        let credentials = await transport.handshakeCredentials
        XCTAssertEqual(credentials, [nil, "issued-session"])
    }

    func testLateCurrentUserSuccessCannotReplaceNewConnection() async throws {
        try await staleResult(failure: nil)
    }
    func testLateUnauthorizedCannotDeleteNewConnectionsSession() async throws {
        try await staleResult(failure: HolonHTTPFailure(statusCode: 401, identity: .init(networkID: "old")))
    }
    private func staleResult(failure: (any Error)?) async throws {
        let f = try fixture()
        let a = try profile(f)
        let b = try profile(f, name: "B")
        try saved(f, profile: a)
        try saved(f, profile: b, credential: "B-session")
        let old = CoordinatorTransport(gate: .user)
        if let failure { await old.failUser(failure) }
        let new = CoordinatorTransport()
        await new.configure(user: "B-user", runtime: "B-runtime", visibility: "B-scope")
        let c = ConnectionCoordinator(store: f.store, proofStore: f.proofs,
                                      makeTransport: { $0.id == a.id ? old : new })
        let pending = Task { await c.connect(a) }
        await old.waitUntilEntered()
        await c.connect(b)
        let identity = try XCTUnwrap(c.identity)
        let session = try XCTUnwrap(f.store.session(for: b))
        await old.release()
        await pending.value
        XCTAssertEqual(c.status, .connected)
        XCTAssertEqual(c.selectedProfile, b)
        XCTAssertEqual(c.identity, identity)
        XCTAssertEqual(try f.store.session(for: b), session)
        XCTAssertEqual(session.credential, "B-session")
        XCTAssertEqual(session.userID, "B-user")
    }

    func testTokenPersistsOnlyIssuedSessionAndRestoresAfterRestart() async throws {
        let f = try fixture()
        let p = try profile(f)
        let transport = CoordinatorTransport(gate: .exchange)
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        XCTAssertEqual(c.status, .needsLogin)
        let pending = Task { await c.login(token: "bootstrap-token") }
        await transport.waitUntilEntered()
        XCTAssertNil(try f.store.session(for: p))
        await transport.release()
        await pending.value
        XCTAssertEqual(c.status, .connected)
        XCTAssertEqual(try f.store.session(for: p)?.credential, "issued-session")
        let requests = await transport.exchanges
        XCTAssertEqual(requests.first?.0, "bootstrap-token")
        XCTAssertNil(requests.first?.1)
        let reopened = try f.reopen()
        let restoredTransport = CoordinatorTransport()
        let restored = ConnectionCoordinator(store: reopened, proofStore: f.proofs,
                                             makeTransport: { _ in restoredTransport })
        await restored.restore()
        XCTAssertEqual(restored.status, .connected)
        let bindings = await restoredTransport.bindings
        XCTAssertEqual(bindings.first??.credential, "issued-session")
        XCTAssertNotEqual(c.identity?.generation, restored.identity?.generation)
        XCTAssertFalse(String(describing: f.defaults.dictionaryRepresentation()).contains("bootstrap-token"))
        XCTAssertFalse(String(describing: f.defaults.dictionaryRepresentation()).contains("issued-session"))
    }

    func testRuntimeUserAndVisibilityChangesPublishNewGeneration() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        for (user, runtime, visibility) in [("user", "runtime", "shared"), ("user", "other-runtime", "shared"), ("other-user", "other-runtime", "shared")] {
            let previous = try XCTUnwrap(c.identity)
            await transport.configure(user: user, runtime: runtime, visibility: visibility)
            await c.refresh()
            let current = try XCTUnwrap(c.identity)
            XCTAssertNotEqual(previous.generation, current.generation)
            XCTAssertEqual(current.userID, user)
            XCTAssertEqual(current.runtimeID, runtime)
            XCTAssertEqual(current.visibilityScopeID, visibility)
            XCTAssertEqual(try f.store.session(for: p)?.userID, user)
        }
    }

    func testReauthenticationBootstrapFailuresClearPreviousIdentity() async throws {
        let cases: [(CoordinatorTransport.BootstrapFailure, ConnectionStatus)] = [
            (.authenticationMode, .networkError), (.incompatibleHandshake, .incompatible),
            (.forbiddenUser, .permissionDenied), (.offlineUser, .networkError)
        ]
        for native in [false, true] {
            for (failure, status) in cases {
                let f = try fixture()
                let p = try profile(f)
                try saved(f, profile: p)
                let transport = CoordinatorTransport(mode: native ? "oidc" : "local")
                await transport.configure(user: "A")
                let c = coordinator(f, transport: transport)
                await c.connect(p)
                XCTAssertEqual(c.status, .connected)
                XCTAssertEqual(c.identity?.userID, "A")
                await transport.configure(user: "B")
                await transport.failBootstrap(failure)
                if native {
                    let proof = try NativeLoginProof.make(apiBaseURL: p.apiBaseURL)
                    try f.proofs.save(proof)
                    await c.handleCallback(try callback(proof))
                } else {
                    await c.login(token: "B-token")
                }
                XCTAssertEqual(c.status, status)
                XCTAssertNil(c.identity, "\(native ? "OIDC" : "token") \(failure)")
                let bindings = await transport.bindings
                XCTAssertEqual(bindings.last??.credential, "issued-session")
                XCTAssertEqual(bindings.last??.userID, "B")
                XCTAssertEqual(try f.store.pendingSession(for: p)?.credential, "issued-session")
                XCTAssertEqual(try f.store.pendingSession(for: p)?.userID, "B")
            }
        }
    }

    func testReauthenticationClearsIdentityUntilNewScopeIsVerified() async throws {
        for native in [false, true] {
            let f = try fixture()
            let p = try profile(f)
            try saved(f, profile: p)
            let transport = CoordinatorTransport(mode: native ? "oidc" : "local")
            await transport.configure(user: "A")
            let c = coordinator(f, transport: transport)
            await c.connect(p)
            let previous = try XCTUnwrap(c.identity)
            await transport.configure(user: "B")
            await transport.block(.user)
            let pending: Task<Void, Never>
            if native {
                let proof = try NativeLoginProof.make(apiBaseURL: p.apiBaseURL)
                try f.proofs.save(proof)
                let url = try callback(proof)
                pending = Task { await c.handleCallback(url) }
            } else {
                pending = Task { await c.login(token: "B-token") }
            }
            await transport.waitUntilEntered()
            let validatingIdentity = c.identity
            let staged = try f.store.pendingSession(for: p)
            await transport.release()
            await pending.value
            XCTAssertNil(validatingIdentity)
            XCTAssertEqual(staged?.userID, "B")
            XCTAssertEqual(c.status, .connected)
            XCTAssertEqual(c.identity?.userID, "B")
            XCTAssertNotEqual(c.identity?.generation, previous.generation)
            XCTAssertEqual(try f.store.session(for: p)?.userID, "B")
            XCTAssertNil(try f.store.pendingSession(for: p))
        }
    }

    func testForbiddenAndNetworkFailuresPreserveSessionButUnauthorizedDeletesIt() async throws {
        let cases: [(any Error, ConnectionStatus, Bool)] = [
            (HolonHTTPFailure(statusCode: 403, identity: .init(networkID: "test")), .permissionDenied, true),
            (URLError(.notConnectedToInternet), .networkError, true),
            (HolonHTTPFailure(statusCode: 401, identity: .init(networkID: "test")), .sessionExpired, false)
        ]
        for (error, status, retained) in cases {
            let f = try fixture()
            let p = try profile(f)
            try saved(f, profile: p)
            let transport = CoordinatorTransport()
            await transport.failUser(error)
            let c = coordinator(f, transport: transport)
            await c.connect(p)
            XCTAssertEqual(c.status, status)
            XCTAssertEqual(try f.store.session(for: p) != nil, retained)
            XCTAssertNil(c.identity)
        }
    }

    func testTimeoutFencesLateSuccessfulResult() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport(gate: .user)
        let c = coordinator(f, transport: transport, deadline: .milliseconds(20))
        let pending = Task { await c.connect(p) }
        await transport.waitUntilEntered()
        // Only the deadline test uses time; the request itself remains explicitly gated.
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(c.status, .timedOut)
        await transport.release()
        await pending.value
        XCTAssertEqual(c.status, .timedOut)
        XCTAssertNil(c.identity)
        XCTAssertEqual(try f.store.session(for: p)?.credential, "saved-session")
    }

    func testLogoutDeletesLocalSessionEvenWhenRevocationFails() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        await transport.failRevocation()
        await c.logout()
        XCTAssertEqual(c.status, .needsLogin)
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.session(for: p))
        XCTAssertNil(try f.reopen().session(for: p))
    }

    func testPairingPreviewIsOfflineAndOnlyConfirmationRedeems() async throws {
        let f = try fixture()
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        let ticket = String(repeating: "a", count: 64)
        try c.previewPairing("https://pair.example.test/login#pair=\(ticket)")
        XCTAssertNotNil(c.pendingPairing)
        XCTAssertNil(c.selectedProfile)
        XCTAssertTrue(c.profiles.isEmpty)
        let before = await transport.redemptions
        let bindingsBefore = await transport.bindings
        XCTAssertTrue(before.isEmpty)
        XCTAssertTrue(bindingsBefore.isEmpty)
        await c.confirmPairing(allowInsecureHTTP: false)
        XCTAssertEqual(c.status, .connected)
        XCTAssertNil(c.pendingPairing)
        let requests = await transport.redemptions
        XCTAssertEqual(requests, [ticket])
        let p = try XCTUnwrap(c.selectedProfile)
        XCTAssertEqual(p.apiBaseURL.host, "pair.example.test")
        XCTAssertEqual(try f.store.session(for: p)?.credential, "issued-session")
    }

    func testPairingHTTPRequiresExplicitConfirmationBeforeAnyRequest() async throws {
        let f = try fixture()
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        let payload = "http://pair.example.test/login#pair=\(String(repeating: "b", count: 64))"
        try c.previewPairing(payload)
        await c.confirmPairing(allowInsecureHTTP: false)
        XCTAssertEqual(c.status, .loginFailed)
        XCTAssertNil(c.selectedProfile)
        XCTAssertTrue(c.profiles.isEmpty)
        let rejected = await transport.redemptions
        XCTAssertTrue(rejected.isEmpty)
        try c.previewPairing(payload)
        await c.confirmPairing(allowInsecureHTTP: true)
        XCTAssertEqual(c.status, .connected)
        XCTAssertEqual(c.selectedProfile?.allowInsecureHTTP, true)
        let accepted = await transport.redemptions
        XCTAssertEqual(accepted.count, 1)
    }

    func testRestartProofLocatorWrongStateAndCorrectCallback() async throws {
        let f = try fixture()
        let p = try profile(f)
        let proof = try NativeLoginProof.make(apiBaseURL: p.apiBaseURL)
        try f.proofs.save(proof)
        let reopenedProofs = NativeLoginProofStore(vault: CredentialVault(service: f.suite + ".proofs"))
        XCTAssertEqual(try reopenedProofs.loadPending(apiBaseURL: p.apiBaseURL), proof)
        let transport = CoordinatorTransport(mode: "oidc")
        let c = ConnectionCoordinator(store: try f.reopen(), proofStore: reopenedProofs,
                                      makeTransport: { _ in transport })
        await c.restore() // Select explicitly below: no saved selection is required by proof storage.
        await c.connect(p)
        XCTAssertTrue(c.supportsOIDC)
        await c.handleCallback(try callback(proof, state: "wrong"))
        let wrongRequests = await transport.exchanges
        XCTAssertTrue(wrongRequests.isEmpty)
        XCTAssertEqual(try reopenedProofs.loadPending(apiBaseURL: p.apiBaseURL), proof)
        await c.handleCallback(try callback(proof))
        XCTAssertEqual(c.status, .connected)
        let requests = await transport.exchanges
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.1, proof.verifier)
        XCTAssertNil(try reopenedProofs.loadPending(apiBaseURL: p.apiBaseURL))
        XCTAssertNil(try reopenedProofs.load(apiBaseURL: p.apiBaseURL, state: proof.state))
        XCTAssertEqual(try f.store.session(for: p)?.credential, "issued-session")
    }

    func testDuplicateCallbackAndCancellationFenceInFlightExchange() async throws {
        let f = try fixture()
        let p = try profile(f)
        let proof = try NativeLoginProof.make(apiBaseURL: p.apiBaseURL)
        try f.proofs.save(proof)
        let transport = CoordinatorTransport(mode: "oidc", gate: .exchange)
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        let url = try callback(proof)
        let pending = Task { await c.handleCallback(url) }
        await transport.waitUntilEntered()
        await c.handleCallback(url)
        let requests = await transport.exchanges
        XCTAssertEqual(requests.count, 1)
        c.cancelLogin()
        XCTAssertNil(try f.proofs.loadPending(apiBaseURL: p.apiBaseURL))
        await c.handleCallback(url)
        await transport.release()
        await pending.value
        XCTAssertEqual(c.status, .needsLogin)
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.session(for: p))
        let finalRequests = await transport.exchanges
        XCTAssertEqual(finalRequests.count, 1)
    }

    func testIssuedCredentialSurvivesBootstrapNetworkFailureAndRestart() async throws {
        let f = try fixture()
        let p = try profile(f)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        await transport.failUser(URLError(.notConnectedToInternet))
        await c.login(token: "bootstrap-token")
        XCTAssertEqual(c.status, .networkError)
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.session(for: p))
        XCTAssertEqual(try f.store.pendingSession(for: p)?.credential, "issued-session")

        let reopened = try f.reopen()
        XCTAssertEqual(try reopened.pendingSession(for: p)?.credential, "issued-session")
        let restoredTransport = CoordinatorTransport()
        let restored = ConnectionCoordinator(store: reopened, proofStore: f.proofs,
                                             makeTransport: { _ in restoredTransport })
        await restored.restore()
        XCTAssertEqual(restored.status, .connected)
        XCTAssertNotNil(restored.identity)
        XCTAssertEqual(try reopened.session(for: p)?.credential, "issued-session")
        XCTAssertNil(try reopened.pendingSession(for: p))
        XCTAssertNil(try f.reopen().pendingSession(for: p))
        let credentials = await restoredTransport.handshakeCredentials
        XCTAssertEqual(credentials, ["issued-session"])
        let exchanges = await restoredTransport.exchanges
        XCTAssertTrue(exchanges.isEmpty)
    }

    func testUncertainNativeExchangeFailurePreservesProof() async throws {
        let f = try fixture()
        let p = try profile(f)
        let proof = try NativeLoginProof.make(apiBaseURL: p.apiBaseURL)
        try f.proofs.save(proof)
        let transport = CoordinatorTransport(mode: "oidc")
        await transport.failExchange(URLError(.networkConnectionLost))
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        await c.handleCallback(try callback(proof))
        XCTAssertEqual(c.status, .networkError)
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.pendingSession(for: p))
        XCTAssertEqual(try f.proofs.loadPending(apiBaseURL: p.apiBaseURL), proof)
        XCTAssertEqual(try f.proofs.load(apiBaseURL: p.apiBaseURL, state: proof.state), proof)
    }

    func testNativeExchangeTimeoutPreservesProofUntilExplicitCancellation() async throws {
        let f = try fixture()
        let p = try profile(f)
        let proof = try NativeLoginProof.make(apiBaseURL: p.apiBaseURL)
        try f.proofs.save(proof)
        let transport = CoordinatorTransport(mode: "oidc", gate: .exchange)
        let c = coordinator(f, transport: transport, deadline: .milliseconds(20))
        await c.connect(p)
        let url = try callback(proof)
        let pending = Task { await c.handleCallback(url) }
        await transport.waitUntilEntered()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(c.status, .timedOut)
        XCTAssertNil(c.identity)
        XCTAssertEqual(try f.proofs.loadPending(apiBaseURL: p.apiBaseURL), proof)
        XCTAssertEqual(try f.proofs.load(apiBaseURL: p.apiBaseURL, state: proof.state), proof)
        c.cancelLogin()
        XCTAssertEqual(c.status, .needsLogin)
        XCTAssertNil(try f.proofs.loadPending(apiBaseURL: p.apiBaseURL))
        XCTAssertNil(try f.proofs.load(apiBaseURL: p.apiBaseURL, state: proof.state))
        await transport.release()
        await pending.value
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.pendingSession(for: p))
        XCTAssertNil(try f.store.session(for: p))
    }

    func testDuplicateCallbackConsumesTicketOnlyOnceOnSuccess() async throws {
        let f = try fixture()
        let p = try profile(f)
        let proof = try NativeLoginProof.make(apiBaseURL: p.apiBaseURL)
        try f.proofs.save(proof)
        let transport = CoordinatorTransport(mode: "oidc", gate: .exchange)
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        let url = try callback(proof)
        let pending = Task { await c.handleCallback(url) }
        await transport.waitUntilEntered()
        await c.handleCallback(url)
        await transport.release()
        await pending.value
        let identity = try XCTUnwrap(c.identity)
        await c.handleCallback(url)
        let requests = await transport.exchanges
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(c.identity, identity)
        XCTAssertEqual(c.status, .connected)
        XCTAssertNil(try f.proofs.loadPending(apiBaseURL: p.apiBaseURL))
    }
}
