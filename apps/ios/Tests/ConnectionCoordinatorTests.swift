import Foundation
import HolonClient
import XCTest
@testable import Holon

/// close deliberately does not release or cancel requests: the coordinator must fence results.
private actor CoordinatorTransport: ConnectionTransport {
    enum Gate { case authenticationMode, user, exchange }
    enum BootstrapFailure { case authenticationMode, incompatibleHandshake, forbiddenUser, offlineUser }
    let mode: String
    let authRequired: Bool
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

    init(mode: String = "local", gate: Gate? = nil, authRequired: Bool = true) {
        self.mode = mode
        self.gate = gate
        self.authRequired = authRequired
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
        await pause(.authenticationMode)
        if bootstrapFailure == .authenticationMode { throw URLError(.notConnectedToInternet) }
        return mode
    }
    func handshake() async throws -> HolonHandshake {
        let credential = bindings.last.flatMap { $0?.credential }
        handshakeCredentials.append(credential)
        guard !authRequired || credential != nil else {
            throw HolonHTTPFailure(statusCode: 401, identity: .init(networkID: "fixture"))
        }
        return try HolonHandshake(data: Data("""
        {"ok":true,"protocol":{"name":"holon-control","version":\(bootstrapFailure == .incompatibleHandshake ? 999 : 1)},
         "auth":{"mode":"\(mode == "oidc" ? "bearer" : "local")","required":\(authRequired)},"capabilities":[],
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

    func testFreshLaunchWaitsForRestoreThenShowsConnection() async throws {
        let f = try fixture()
        let c = coordinator(f, transport: CoordinatorTransport())
        XCTAssertEqual(c.launchState, .restoring)
        await c.restore()
        XCTAssertEqual(c.launchState, .connection)
        XCTAssertNil(c.identity)
        XCTAssertTrue(c.hasRestored)
    }

    func testColdOfflineRestorePublishesReadOnlyScopeBeforeRequestFinishes() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        try f.store.select(p.id)
        let transport = CoordinatorTransport(gate: .authenticationMode)
        await transport.failBootstrap(.authenticationMode)
        let c = coordinator(f, transport: transport)
        var purged: [ReadingPartition] = []
        c.onReadingInvalidation = { purged.append($0) }
        let restoration = Task { await c.restore() }
        await transport.waitUntilEntered()
        let context = try XCTUnwrap(c.readingContext)
        XCTAssertEqual(context.profile, p)
        XCTAssertEqual(context.partition.network, p.id.uuidString)
        XCTAssertEqual(c.launchState, .ready)
        XCTAssertEqual(c.status, .connecting)
        XCTAssertFalse(c.hasRestored)
        XCTAssertNil(c.identity)
        do {
            _ = try await c.makeAuthenticatedClient()
            XCTFail("A restored local scope is not online authority")
        } catch { XCTAssertEqual(error as? HolonClientError, .staleConnection) }
        await transport.release()
        await restoration.value
        XCTAssertEqual(c.status, .networkError)
        XCTAssertEqual(c.readingContext, context)
        XCTAssertEqual(c.launchState, .ready)
        XCTAssertNil(c.identity)
        XCTAssertTrue(purged.isEmpty)
        await transport.block(.authenticationMode)
        let retry = Task { await c.refresh() }
        await transport.waitUntilEntered()
        XCTAssertEqual(c.readingContext, context)
        XCTAssertEqual(c.launchState, .ready, "Retrying offline bootstrap must not restore a launch-screen gate")
        XCTAssertNil(c.identity)
        await transport.release()
        await retry.value
        XCTAssertEqual(c.readingContext, context)
        XCTAssertEqual(c.status, .networkError)
        XCTAssertTrue(purged.isEmpty)
    }

    func testExpiredCredentialRestoresOnlyLocalScopeAndClearsExtensionCapability() async throws {
        let f = try fixture()
        let p = try profile(f)
        try f.store.saveSession(.init(credential: "expired", runtimeID: "runtime", userID: "user",
            visibilityScopeID: "private", expiresAt: .distantPast), for: p)
        try f.store.select(p.id)
        let group = try SharedSessionVault.configured().accessGroup
        let sharing = SharedSessionVault(accessGroup: group, service: "test.offline." + UUID().uuidString)
        defer { try? sharing.clear() }
        try sharing.write(.init(generation: UUID(), networkID: p.id.uuidString,
            connectionName: p.name, apiBaseURL: p.apiBaseURL, allowInsecureHTTP: p.allowInsecureHTTP,
            runtimeID: "runtime", userID: "user", visibilityScopeID: "private",
            credential: "old-capability", expiresAt: nil))
        let transport = CoordinatorTransport(mode: "oidc", gate: .authenticationMode)
        await transport.failBootstrap(.authenticationMode)
        let c = ConnectionCoordinator(store: f.store, sharingVault: sharing, proofStore: f.proofs,
                                      makeTransport: { _ in transport })
        let restoration = Task { await c.restore() }
        await transport.waitUntilEntered()
        XCTAssertNotNil(c.readingContext)
        XCTAssertEqual(c.launchState, .ready)
        XCTAssertNil(c.identity)
        XCTAssertNil(try sharing.read())
        XCTAssertNil(try f.store.session(for: p))
        do {
            _ = try await c.makeAuthenticatedClient()
            XCTFail("Expired credentials must never enable a client")
        } catch { XCTAssertEqual(error as? HolonClientError, .staleConnection) }
        await transport.release()
        await restoration.value
        XCTAssertEqual(c.status, .networkError)
        XCTAssertNotNil(c.readingContext)
        XCTAssertNil(try sharing.read())
    }

    func testSameProfileReconnectPreservesReadingScopeWhileWithdrawingOnlineIdentity() async throws {
        let cases: [(CoordinatorTransport.BootstrapFailure?, ConnectionStatus)] = [
            (nil, .connected), (.authenticationMode, .networkError), (nil, .timedOut)
        ]
        for (failure, expectedStatus) in cases {
            let f = try fixture()
            let p = try profile(f)
            try saved(f, profile: p)
            let transport = CoordinatorTransport()
            let c = coordinator(f, transport: transport,
                                deadline: expectedStatus == .timedOut ? .milliseconds(30) : .seconds(30))
            await c.connect(p)
            let previousIdentity = try XCTUnwrap(c.identity)
            let context = try XCTUnwrap(c.readingContext)
            var withdrawals: [ConfirmedReadingContext?] = []
            var purged: [ReadingPartition] = []
            c.onIdentityChange = { [weak c] in
                guard let c, c.identity == nil else { return }
                withdrawals.append(c.readingContext)
                XCTAssertEqual(c.readingContext, context)
                XCTAssertEqual(c.launchState, .ready)
            }
            c.onReadingInvalidation = { purged.append($0) }
            if let failure { await transport.failBootstrap(failure) }
            await transport.block(.authenticationMode)
            let retry = Task { await c.refresh() }
            await transport.waitUntilEntered()
            XCTAssertEqual(c.readingContext, context)
            XCTAssertEqual(c.launchState, .ready)
            XCTAssertNil(c.identity)
            XCTAssertEqual(withdrawals, [context])
            do {
                _ = try await c.makeAuthenticatedClient()
                XCTFail("Preserved reading context must not preserve online authority")
            } catch { XCTAssertEqual(error as? HolonClientError, .staleConnection) }
            if expectedStatus == .timedOut {
                try await Task.sleep(for: .milliseconds(100))
                XCTAssertEqual(c.status, .timedOut)
                XCTAssertEqual(c.readingContext, context)
                XCTAssertEqual(c.launchState, .ready)
            }
            await transport.release()
            await retry.value
            XCTAssertEqual(c.status, expectedStatus)
            XCTAssertEqual(c.readingContext, context)
            XCTAssertEqual(c.launchState, .ready)
            XCTAssertTrue(purged.isEmpty)
            XCTAssertEqual(try f.store.confirmedReadingContext(for: p), context)
            if expectedStatus == .connected {
                XCTAssertNotNil(c.identity)
                XCTAssertNotEqual(c.identity?.generation, previousIdentity.generation)
            } else {
                XCTAssertNil(c.identity)
            }
        }
    }

    func testSameProfileReconnectWithdrawsScopeBeforeNotifyingForChangedLocalContext() async throws {
        for change in ["network", "runtime", "user", "visibility", "staged-login"] {
            let stagedLogin = change == "staged-login"
            let f = try fixture()
            let p = try profile(f)
            try saved(f, profile: p)
            let transport = CoordinatorTransport(mode: "oidc")
            let c = coordinator(f, transport: transport)
            await c.connect(p)
            let previous = try XCTUnwrap(c.readingContext)
            let replacement = try XCTUnwrap(ConfirmedReadingContext(profile: p, identity: .init(
                networkID: change == "network" ? "other-network" : previous.partition.network,
                runtimeID: change == "runtime" ? "other-runtime" : previous.partition.runtime,
                userID: change == "user" ? "other-user" : previous.partition.user,
                visibilityScopeID: change == "visibility" ? "other-private" : previous.partition.visibility)))
            if stagedLogin {
                try f.store.stageSession(.init(apiBaseURL: p.apiBaseURL, credential: "new-account",
                                              userID: "other-user", expiresAt: nil), for: p)
            } else {
                try f.store.saveConfirmedReadingContext(replacement, for: p)
            }
            var withdrawals = 0
            c.onIdentityChange = { [weak c] in
                guard let c, c.identity == nil else { return }
                withdrawals += 1
                XCTAssertNil(c.readingContext, "A changed \(change) must be withdrawn before identity observers run")
            }
            await transport.block(.authenticationMode)
            await transport.failBootstrap(.authenticationMode)
            let retry = Task { await c.refresh() }
            await transport.waitUntilEntered()
            XCTAssertEqual(withdrawals, 1)
            XCTAssertNil(c.identity)
            if stagedLogin {
                XCTAssertNil(c.readingContext)
                XCTAssertNil(try f.store.confirmedReadingContext(for: p))
            } else {
                XCTAssertEqual(c.readingContext, replacement)
                XCTAssertNotEqual(c.readingContext?.partition, previous.partition)
            }
            await transport.release()
            await retry.value
            XCTAssertEqual(c.status, .networkError)
        }
    }

    func testSameProfileReconnectPermissionFailurePurgesRetainedScope() async throws {
        for code in [401, 403] {
            let f = try fixture()
            let p = try profile(f)
            try saved(f, profile: p)
            let transport = CoordinatorTransport()
            let c = coordinator(f, transport: transport)
            await c.connect(p)
            let context = try XCTUnwrap(c.readingContext)
            var withdrawals: [ConfirmedReadingContext?] = []
            var purged: [ReadingPartition] = []
            c.onIdentityChange = { [weak c] in
                guard let c, c.identity == nil else { return }
                withdrawals.append(c.readingContext)
            }
            c.onReadingInvalidation = { [weak c] in
                purged.append($0)
                XCTAssertNil(c?.readingContext)
                XCTAssertNil(c?.identity)
            }
            await transport.failUser(HolonHTTPFailure(statusCode: code, identity: .init(networkID: "fixture")))
            await c.refresh()
            XCTAssertEqual(withdrawals, [context])
            XCTAssertEqual(c.status, code == 401 ? .sessionExpired : .permissionDenied)
            XCTAssertNil(c.identity)
            XCTAssertNil(c.readingContext)
            XCTAssertEqual(purged, [context.partition])
            XCTAssertNil(try f.store.confirmedReadingContext(for: p))
            XCTAssertNil(try f.store.session(for: p))
        }
    }

    func testSameProfileReconnectWithUnreadableScopeWithdrawsBeforeIdentityNotification() async throws {
        for startOffline in [false, true] {
            let f = try fixture()
            let p = try profile(f)
            try saved(f, profile: p)
            try f.store.select(p.id)
            let transport = CoordinatorTransport()
            if startOffline { await transport.failBootstrap(.authenticationMode) }
            let c = coordinator(f, transport: transport)
            await c.restore()
            let previous = try XCTUnwrap(c.readingContext)
            let previousBindings = await transport.bindings.count
            XCTAssertEqual(c.identity == nil, startOffline)
            try f.vault.write(Data("not-json".utf8), account: "confirmed-reading.\(p.id.uuidString)")
            var withdrawals = 0
            c.onIdentityChange = { [weak c] in
                guard let c, c.identity == nil else { return }
                withdrawals += 1
                XCTAssertNil(c.readingContext)
                XCTAssertNil(c.readingContext?.partition)
            }
            await c.refresh()
            XCTAssertEqual(withdrawals, startOffline ? 0 : 1)
            XCTAssertEqual(c.status, .storageError)
            XCTAssertNil(c.identity)
            XCTAssertNil(c.readingContext)
            XCTAssertNil(c.readingContext?.partition)
            XCTAssertNotEqual(c.readingContext?.partition, previous.partition)
            XCTAssertEqual(c.launchState, .connection)
            let bindings = await transport.bindings.count
            XCTAssertEqual(bindings, previousBindings, "Unreadable scope must fail before binding a reconnect session")
            do {
                _ = try await c.makeAuthenticatedClient()
                XCTFail("Unreadable scope must not retain online authority")
            } catch { XCTAssertEqual(error as? HolonClientError, .staleConnection) }
        }
    }

    func testProfileSwitchWithdrawsPreviousScopeBeforeIdentityNotification() async throws {
        let f = try fixture()
        let a = try profile(f)
        let b = try profile(f, name: "B")
        try saved(f, profile: a)
        let target = try XCTUnwrap(ConfirmedReadingContext(profile: b, identity: .init(
            networkID: b.id.uuidString, runtimeID: "B-runtime", userID: "B-user",
            visibilityScopeID: "B-private")))
        try f.store.saveConfirmedReadingContext(target, for: b)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        await c.connect(a)
        let previous = try XCTUnwrap(c.readingContext)
        var withdrawals = 0
        var purged: [ReadingPartition] = []
        c.onIdentityChange = { [weak c] in
            guard let c, c.identity == nil else { return }
            withdrawals += 1
            XCTAssertNil(c.readingContext)
        }
        c.onReadingInvalidation = { purged.append($0) }
        await transport.block(.authenticationMode)
        await transport.failBootstrap(.authenticationMode)
        let switching = Task { await c.connect(b) }
        await transport.waitUntilEntered()
        XCTAssertEqual(withdrawals, 1)
        XCTAssertEqual(c.selectedProfile, b)
        XCTAssertEqual(c.readingContext, target)
        XCTAssertNil(c.identity)
        XCTAssertEqual(try f.store.confirmedReadingContext(for: a), previous)
        XCTAssertTrue(purged.isEmpty, "Switching nodes must not delete the old node's cache")
        await transport.release()
        await switching.value
        XCTAssertEqual(c.status, .networkError)
        XCTAssertEqual(c.readingContext, target)
    }

    func testAnonymousConfirmationPersistsScopeForNextOfflineLaunch() async throws {
        let f = try fixture()
        let p = try profile(f)
        let c = coordinator(f, transport: CoordinatorTransport(authRequired: false))
        await c.connect(p)
        XCTAssertEqual(c.status, .connected)
        let confirmed = try XCTUnwrap(c.readingContext)
        XCTAssertNotNil(c.identity)
        XCTAssertNil(try f.store.session(for: p))
        XCTAssertEqual(try f.reopen().confirmedReadingContext(for: p), confirmed)
        let offline = CoordinatorTransport()
        await offline.failBootstrap(.authenticationMode)
        let reopened = ConnectionCoordinator(store: try f.reopen(), proofStore: f.proofs,
                                             makeTransport: { _ in offline })
        await reopened.restore()
        XCTAssertEqual(reopened.readingContext, confirmed)
        XCTAssertEqual(reopened.launchState, .ready)
        XCTAssertEqual(reopened.status, .networkError)
        XCTAssertNil(reopened.identity)
    }

    func testProfileSwitchRestoresOnlyTargetsConfirmedLocalContextWithoutPurgingOtherProfile() async throws {
        let f = try fixture()
        let a = try profile(f)
        let b = try profile(f, name: "B")
        let fresh = try profile(f, name: "C")
        let first = try XCTUnwrap(ConfirmedReadingContext(profile: a, identity: .init(
            networkID: "A-network", runtimeID: "A-runtime", userID: "A-user", visibilityScopeID: "A-private")))
        let second = try XCTUnwrap(ConfirmedReadingContext(profile: b, identity: .init(
            networkID: "B-network", runtimeID: "B-runtime", userID: "B-user", visibilityScopeID: "B-private")))
        try f.store.saveConfirmedReadingContext(first, for: a)
        try f.store.saveConfirmedReadingContext(second, for: b)
        let offline = CoordinatorTransport()
        await offline.failBootstrap(.authenticationMode)
        let c = coordinator(f, transport: offline)
        var purged: [ReadingPartition] = []
        c.onReadingInvalidation = { purged.append($0) }
        await c.connect(a)
        XCTAssertEqual(c.readingContext, first)
        await c.connect(b)
        XCTAssertEqual(c.readingContext, second)
        XCTAssertNil(c.identity)
        await c.connect(fresh)
        XCTAssertNil(c.readingContext)
        XCTAssertEqual(try f.store.confirmedReadingContext(for: a), first)
        XCTAssertEqual(try f.store.confirmedReadingContext(for: b), second)
        XCTAssertTrue(purged.isEmpty)
    }

    func testPendingAccountNeverRestoresPreviousReadingScopeBeforeBootstrap() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        _ = try f.store.confirmedReadingContext(for: p)
        try f.store.stageSession(.init(apiBaseURL: p.apiBaseURL, credential: "new-account",
                                      userID: "other-user", expiresAt: nil), for: p)
        try f.store.select(p.id)
        let transport = CoordinatorTransport(mode: "oidc", gate: .authenticationMode)
        await transport.failBootstrap(.authenticationMode)
        let c = coordinator(f, transport: transport)
        let restoration = Task { await c.restore() }
        await transport.waitUntilEntered()
        XCTAssertNil(c.readingContext)
        XCTAssertNil(c.identity)
        XCTAssertEqual(c.launchState, .restoring)
        await transport.release()
        await restoration.value
        XCTAssertEqual(c.launchState, .connection)
        XCTAssertNil(try f.reopen().confirmedReadingContext(for: p))
    }

    func testUnauthorizedAndForbiddenWithdrawIdentityAndPurgeConfirmedReadingPartition() async throws {
        let unknownError = try HolonAPIError(data: Data(
            #"{"ok":false,"code":"future_error","error":"Unauthorized"}"#.utf8))
        for (code, apiError) in [(401, nil), (403, nil), (401, unknownError), (403, unknownError)] {
            let f = try fixture()
            let p = try profile(f)
            try saved(f, profile: p)
            let c = coordinator(f, transport: CoordinatorTransport())
            await c.connect(p)
            let identity = try XCTUnwrap(c.identity)
            let partition = try XCTUnwrap(c.readingContext).partition
            var purged: [ReadingPartition] = []
            var withdrewIdentity = false
            c.onIdentityChange = { [weak c] in
                guard let c, c.identity == nil else { return }
                withdrewIdentity = true
                XCTAssertNil(c.readingContext)
            }
            c.onReadingInvalidation = { [weak c] in
                purged.append($0)
                XCTAssertNil(c?.readingContext)
                XCTAssertNil(c?.identity)
            }
            c.handleClientFailure(.init(statusCode: code, apiError: apiError, identity: identity),
                                  expectedIdentity: identity)
            XCTAssertEqual(c.status, code == 401 ? .sessionExpired : .permissionDenied)
            XCTAssertNil(c.identity)
            XCTAssertNil(c.readingContext)
            XCTAssertNil(try f.reopen().confirmedReadingContext(for: p))
            XCTAssertNil(try f.store.session(for: p))
            XCTAssertEqual(purged, [partition])
            XCTAssertTrue(withdrewIdentity)
        }
    }

    func testLogoutDeletesUnreadableReadingContextAndCredentials() async throws {
        try await verifyUnreadableReadingContextRemoval(removingProfile: false)
    }

    func testRemoveProfileDeletesUnreadableReadingContextAndCredentials() async throws {
        try await verifyUnreadableReadingContextRemoval(removingProfile: true)
    }

    private func verifyUnreadableReadingContextRemoval(removingProfile: Bool) async throws {
        for mismatchedProfile in [false, true] {
            let f = try fixture()
            let p = try profile(f)
            let other = try profile(f, name: "B")
            try saved(f, profile: p)
            try saved(f, profile: other)
            let otherContext = try XCTUnwrap(f.store.confirmedReadingContext(for: other))
            let invalid = mismatchedProfile ? try JSONEncoder().encode(otherContext) : Data("corrupt".utf8)
            let account = "confirmed-reading.\(p.id.uuidString)"
            try f.vault.write(invalid, account: account)
            try f.store.select(p.id)
            let c = coordinator(f, transport: CoordinatorTransport())
            await c.restore()
            XCTAssertEqual(c.status, .storageError)
            XCTAssertNil(c.readingContext)
            XCTAssertNil(c.identity)

            if removingProfile {
                await c.removeProfile(p)
                XCTAssertEqual(c.status, .disconnected)
                XCTAssertNil(c.selectedProfile)
                XCTAssertFalse(c.profiles.contains(p))
                XCTAssertFalse(try f.reopen().profiles.contains(p))
            } else {
                await c.logout()
                XCTAssertEqual(c.status, .needsLogin)
                XCTAssertNil(try f.reopen().session(for: p))
                XCTAssertNil(try f.reopen().confirmedReadingContext(for: p))
            }
            XCTAssertNil(try f.vault.read(account: account))
            XCTAssertNil(c.readingContext)
            XCTAssertNil(c.identity)
            XCTAssertEqual(try f.reopen().confirmedReadingContext(for: other), otherContext)
            XCTAssertNotNil(try f.reopen().session(for: other))
        }
    }

    func testInvalidReadingStorageAndFailedScopePromotionDoNotPublishAuthority() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        try f.vault.write(Data("corrupt".utf8), account: "confirmed-reading.\(p.id.uuidString)")
        await c.connect(p)
        XCTAssertEqual(c.status, .storageError)
        XCTAssertNil(c.readingContext)
        XCTAssertNil(c.identity)
        let bindings = await transport.bindings
        XCTAssertTrue(bindings.isEmpty)
        try f.store.removeSession(for: p)
        try saved(f, profile: p)
        await transport.configure(runtime: "")
        await c.connect(p)
        XCTAssertEqual(c.status, .storageError)
        XCTAssertNil(c.readingContext)
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.confirmedReadingContext(for: p))
    }

    func testSavedLaunchExposesOnlyReadingContextBeforeIdentityIsConfirmed() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        try f.store.select(p.id)
        let transport = CoordinatorTransport(gate: .user)
        let c = coordinator(f, transport: transport)
        var promotedContext: ConfirmedReadingContext?
        var promotions = 0
        c.onIdentityChange = { [weak c] in
            guard let c, let identity = c.identity else { return }
            promotedContext = c.readingContext
            promotions += 1
            XCTAssertEqual(c.readingContext?.profile, p)
            XCTAssertEqual(c.readingContext?.partition,
                           ReadingPartition(apiBaseURL: p.apiBaseURL, identity: identity))
        }
        let restoration = Task { await c.restore() }
        await transport.waitUntilEntered()
        XCTAssertEqual(c.launchState, .ready)
        XCTAssertNotNil(c.readingContext)
        XCTAssertNil(c.identity)
        XCTAssertNil(promotedContext)
        // A second appearance cannot prematurely finish the first restoration.
        await c.restore()
        XCTAssertEqual(c.launchState, .ready)
        await transport.release()
        await restoration.value
        XCTAssertEqual(c.launchState, .ready)
        let identity = try XCTUnwrap(c.identity)
        XCTAssertEqual(promotedContext, c.readingContext)
        XCTAssertEqual(promotions, 1)
        await c.restore()
        XCTAssertEqual(c.identity, identity)
        XCTAssertEqual(promotions, 1, "Repeated appearance must not restart online activation")
        c.handleClientFailure(HolonHTTPFailure(statusCode: 503, identity: identity), expectedIdentity: identity)
        XCTAssertEqual(c.launchState, .ready, "Transient offline state retains confirmed identity")
        c.handleClientFailure(HolonHTTPFailure(statusCode: 401, identity: identity), expectedIdentity: identity)
        XCTAssertEqual(c.launchState, .connection, "Revoked authority returns to sign-in")
        XCTAssertNil(c.readingContext)
    }

    func testBootstrapDeadlineReleasesLaunchScreenBeforeTransportReturns() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        try f.store.select(p.id)
        let transport = CoordinatorTransport(gate: .user)
        let c = coordinator(f, transport: transport, deadline: .milliseconds(30))
        var purged: [ReadingPartition] = []
        c.onReadingInvalidation = { purged.append($0) }
        let restoration = Task { await c.restore() }
        await transport.waitUntilEntered()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(c.status, .timedOut)
        XCTAssertEqual(c.launchState, .ready)
        XCTAssertNotNil(c.readingContext)
        XCTAssertNil(c.identity)
        await transport.release()
        await restoration.value
        XCTAssertEqual(c.launchState, .ready)
        XCTAssertNil(c.identity, "Late success cannot pass the expired bootstrap")
        XCTAssertTrue(purged.isEmpty, "A transport deadline is not permission revocation")
    }

    func testFailedSavedLaunchKeepsHostAndOffersRecoveryWithoutIdentity() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        try f.store.select(p.id)
        let transport = CoordinatorTransport()
        await transport.failBootstrap(.authenticationMode)
        let c = coordinator(f, transport: transport)
        await c.restore()
        XCTAssertEqual(c.launchState, .ready)
        XCTAssertNotNil(c.readingContext)
        XCTAssertEqual(c.status, .networkError)
        XCTAssertEqual(c.selectedProfile, p)
        XCTAssertNil(c.identity)
    }

    func testPairingCancellationIsOfflineAndDoesNotInstallAProfile() async throws {
        let f = try fixture()
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        await c.restore()
        try c.previewPairing("https://pair.test/login#pair=\(String(repeating: "a", count: 64))")
        c.cancelPairing()
        await c.confirmPairing(allowInsecureHTTP: false)
        XCTAssertNil(c.pendingPairing)
        XCTAssertTrue(c.profiles.isEmpty)
        XCTAssertEqual(c.launchState, .connection)
        let requests = await transport.redemptions
        XCTAssertTrue(requests.isEmpty)
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

    func testRuntimeBrowserLeaseRequiresConnectedAuthority() async throws {
        let f = try fixture()
        let p = try profile(f)
        let c = coordinator(f, transport: CoordinatorTransport())
        XCTAssertNil(c.runtimeBrowserLease())
        await c.connect(p)
        XCTAssertEqual(c.status, .needsLogin)
        XCTAssertNil(c.runtimeBrowserLease())
        try saved(f, profile: p)
        await c.connect(p)
        XCTAssertEqual(c.status, .connected)
        let lease = try XCTUnwrap(c.runtimeBrowserLease())
        XCTAssertTrue(c.isRuntimeBrowserLeaseCurrent(lease))
    }

    func testBindRuntimeBrowserCookieBindsHttpOnlySessionHeader() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(p)
        let lease = try XCTUnwrap(c.runtimeBrowserLease())
        let header = try XCTUnwrap(try c.bindRuntimeBrowserCookie(lease, url: "https://a.example.test/settings").get())
        XCTAssertEqual(header, "holon_session=saved-session; Path=/; HttpOnly; SameSite=Lax; Secure")
        if case .failure(let failure) = c.bindRuntimeBrowserCookie(lease, url: "https://other.example.test/x") {
            XCTAssertEqual(failure, .foreignOrigin)
        } else {
            XCTFail("A page outside the runtime origin must not receive the cookie")
        }
    }

    func testBindRuntimeBrowserCookieRejectsInvalidAndMissingCredentials() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p, credential: "bad;value")
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(p)
        let lease = try XCTUnwrap(c.runtimeBrowserLease())
        if case .failure(let failure) = c.bindRuntimeBrowserCookie(lease, url: "https://a.example.test/settings") {
            XCTAssertEqual(failure, .invalidCredential)
        } else {
            XCTFail("A credential with cookie delimiters must never become a Set-Cookie value")
        }

        try f.store.removeSession(for: p)
        let reconnected = coordinator(f, transport: CoordinatorTransport())
        try saved(f, profile: p)
        await reconnected.connect(p)
        XCTAssertEqual(reconnected.status, .connected)
        let validLease = try XCTUnwrap(reconnected.runtimeBrowserLease())
        try f.store.removeSession(for: validLease.profile)
        if case .failure(let failure) = reconnected.bindRuntimeBrowserCookie(validLease, url: "https://a.example.test/settings") {
            XCTAssertEqual(failure, .credentialMissing)
        } else {
            XCTFail("A lease without a stored session credential must not bind a cookie")
        }
    }

    func testBindRuntimeBrowserCookieFailsOnceTheLeaseIsStale() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(p)
        let stale = try XCTUnwrap(c.runtimeBrowserLease())
        await c.logout()
        XCTAssertNil(c.runtimeBrowserLease())
        XCTAssertFalse(c.isRuntimeBrowserLeaseCurrent(stale))
        if case .failure(let failure) = c.bindRuntimeBrowserCookie(stale, url: "https://a.example.test/settings") {
            XCTAssertEqual(failure, .leaseChanged)
        } else {
            XCTFail("A retired lease must not bind cookies")
        }

        // A→B→A: reconnecting the same profile issues a fresh identity generation.
        // logout deleted the stored credential, so save a fresh one first.
        try saved(f, profile: p)
        await c.connect(p)
        XCTAssertEqual(c.status, .connected)
        XCTAssertFalse(c.isRuntimeBrowserLeaseCurrent(stale))
        XCTAssertNotEqual(try XCTUnwrap(c.runtimeBrowserLease()), stale)
    }

    func testRuntimeBrowserAuthenticationFailedRevokesSharedSessionLifecycle() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(p)
        let lease = try XCTUnwrap(c.runtimeBrowserLease())
        c.runtimeBrowserAuthenticationFailed(lease)
        XCTAssertEqual(c.status, .sessionExpired)
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.session(for: p))
        XCTAssertFalse(c.isRuntimeBrowserLeaseCurrent(lease))
        XCTAssertNil(c.runtimeBrowserLease())
    }

    func testReadingClientRequiresConfirmedIdentityAndUsesAnIndependentGeneration() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let c = coordinator(f, transport: CoordinatorTransport())
        do {
            _ = try await c.makeReadingClient()
            XCTFail("A disconnected profile is not a reading authority")
        } catch {
            XCTAssertEqual(error as? HolonClientError, .staleConnection)
        }
        await c.connect(p)
        let identity = try XCTUnwrap(c.identity)
        let reader = try await c.makeReadingClient()
        let readingIdentity = await reader.identity
        XCTAssertEqual(readingIdentity.networkID, identity.networkID)
        XCTAssertEqual(readingIdentity.runtimeID, identity.runtimeID)
        XCTAssertEqual(readingIdentity.userID, identity.userID)
        XCTAssertEqual(readingIdentity.visibilityScopeID, identity.visibilityScopeID)
        XCTAssertNotEqual(readingIdentity.generation, identity.generation)
        await reader.close()
    }

    func testSendingClientLifetimeIsIndependentFromReadingClient() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(p)
        let identity = try XCTUnwrap(c.identity)
        let reader = try await c.makeReadingClient()
        let sender = try await c.makeAuthenticatedClient()
        let readingIdentity = await reader.identity
        let sendingIdentity = await sender.identity
        XCTAssertNotEqual(readingIdentity.generation, sendingIdentity.generation)
        XCTAssertEqual(sendingIdentity.runtimeID, identity.runtimeID)
        XCTAssertEqual(sendingIdentity.userID, identity.userID)
        XCTAssertEqual(sendingIdentity.visibilityScopeID, identity.visibilityScopeID)
        await reader.close()
        let stillBound = await sender.identity
        XCTAssertEqual(stillBound, sendingIdentity)
        await sender.close()
    }

    func testReadingAuthorityIsRevokedSynchronouslyBeforeReauthentication() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        await c.connect(p)
        var invalidated = false
        let partition = try XCTUnwrap(c.readingContext).partition
        var purged: [ReadingPartition] = []
        c.onReadingInvalidation = { [weak c] in
            purged.append($0)
            XCTAssertNil(c?.readingContext)
            XCTAssertNil(c?.identity)
        }
        c.onIdentityChange = { [weak c] in
            guard let c, c.identity == nil else { return }
            invalidated = true
            XCTAssertNil(c.readingContext)
        }
        await transport.block(.exchange)
        let login = Task { await c.login(token: "new-token") }
        await transport.waitUntilEntered()
        XCTAssertTrue(invalidated)
        XCTAssertNil(c.identity)
        XCTAssertNil(c.readingContext)
        XCTAssertNil(try f.store.confirmedReadingContext(for: p))
        XCTAssertEqual(purged, [partition])
        await transport.release()
        await login.value
        c.onIdentityChange = nil
    }

    func testReadingUnauthorizedFromAnOldAuthorityCannotRevokeNewSession() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(p)
        let previous = try XCTUnwrap(c.identity)
        await c.refresh()
        let current = try XCTUnwrap(c.identity)
        XCTAssertNotEqual(previous.generation, current.generation)
        let failure = HolonHTTPFailure(statusCode: 401, identity: .init(networkID: "reading"))
        c.handleReadingFailure(failure, expectedIdentity: previous)
        XCTAssertEqual(c.identity, current)
        XCTAssertEqual(c.status, .connected)
        XCTAssertNotNil(try f.store.session(for: p))
        c.handleReadingFailure(failure, expectedIdentity: current)
        XCTAssertNil(c.identity)
        XCTAssertEqual(c.status, .sessionExpired)
        XCTAssertNil(try f.store.session(for: p))
    }

    func testExpiredStoredSessionCannotFallBackToAnonymousReading() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(p)
        let confirmed = try XCTUnwrap(c.readingContext)
        var offlineObservations = 0
        var purged: [ReadingPartition] = []
        c.onIdentityChange = { [weak c] in
            XCTAssertNil(c?.identity)
            XCTAssertEqual(c?.readingContext, confirmed)
            offlineObservations += 1
        }
        c.onReadingInvalidation = { purged.append($0) }
        let session = try XCTUnwrap(f.store.session(for: p))
        try f.store.saveSession(StoredSession(
            credential: session.credential, runtimeID: session.runtimeID, userID: session.userID,
            visibilityScopeID: session.visibilityScopeID, expiresAt: .distantPast
        ), for: p)
        do {
            _ = try await c.makeReadingClient()
            XCTFail("An expired session must require sign-in")
        } catch {
            XCTAssertEqual(error as? HolonClientError, .staleConnection)
        }
        XCTAssertNil(c.identity)
        XCTAssertEqual(c.status, .sessionExpired)
        XCTAssertNotNil(c.readingContext, "Credential expiry alone does not revoke local content")
        XCTAssertEqual(try f.store.confirmedReadingContext(for: p), c.readingContext)
        XCTAssertEqual(offlineObservations, 1, "The App observer must receive the retained read-only context")
        XCTAssertTrue(purged.isEmpty)
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
        var purged: [ReadingPartition] = []
        c.onReadingInvalidation = { purged.append($0) }
        for (user, runtime, visibility) in [("user", "runtime", "shared"), ("user", "other-runtime", "shared"), ("other-user", "other-runtime", "shared")] {
            let previous = try XCTUnwrap(c.identity)
            let previousPartition = try XCTUnwrap(c.readingContext).partition
            purged.removeAll()
            await transport.configure(user: user, runtime: runtime, visibility: visibility)
            await c.refresh()
            let current = try XCTUnwrap(c.identity)
            XCTAssertNotEqual(previous.generation, current.generation)
            XCTAssertEqual(current.userID, user)
            XCTAssertEqual(current.runtimeID, runtime)
            XCTAssertEqual(current.visibilityScopeID, visibility)
            XCTAssertEqual(try f.store.session(for: p)?.userID, user)
            let context = try XCTUnwrap(c.readingContext)
            XCTAssertEqual(context.partition.user, user)
            XCTAssertEqual(context.partition.runtime, runtime)
            XCTAssertEqual(context.partition.visibility, visibility)
            XCTAssertEqual(try f.store.confirmedReadingContext(for: p), context)
            XCTAssertEqual(purged, [previousPartition])
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
                XCTAssertEqual(try f.store.pendingSession(for: p)?.credential,
                               failure == .forbiddenUser ? nil : "issued-session")
                XCTAssertEqual(try f.store.pendingSession(for: p)?.userID,
                               failure == .forbiddenUser ? nil : "B")
                XCTAssertNil(c.readingContext)
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

    func testOnlyNetworkFailurePreservesConfirmedSessionAndReadingContext() async throws {
        let cases: [(any Error, ConnectionStatus, Bool)] = [
            (HolonHTTPFailure(statusCode: 403, identity: .init(networkID: "test")), .permissionDenied, false),
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
            XCTAssertEqual(c.readingContext != nil, retained)
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
        let partition = try XCTUnwrap(c.readingContext).partition
        var purged: [ReadingPartition] = []
        var withdrewIdentity = false
        c.onIdentityChange = { [weak c] in
            guard let c, c.identity == nil else { return }
            withdrewIdentity = true
            XCTAssertNil(c.readingContext)
        }
        c.onReadingInvalidation = { [weak c] in
            purged.append($0)
            XCTAssertNil(c?.readingContext)
            XCTAssertNil(c?.identity)
        }
        await transport.failRevocation()
        await c.logout()
        XCTAssertEqual(c.status, .needsLogin)
        XCTAssertNil(c.identity)
        XCTAssertNil(try f.store.session(for: p))
        XCTAssertNil(try f.reopen().session(for: p))
        XCTAssertNil(c.readingContext)
        XCTAssertNil(try f.reopen().confirmedReadingContext(for: p))
        XCTAssertEqual(purged, [partition])
        XCTAssertTrue(withdrewIdentity)
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
        XCTAssertEqual(c.pairingTargetURL?.absoluteString, "http://pair.example.test/api/")
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

    func testPairingConsentUsesExactReusedProfileURLWithoutMutatingPreview() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        let consent = SharingConsent(defaults: f.defaults)
        try c.previewPairing("https://a.example.test/login#pair=\(String(repeating: "a", count: 64))")
        let invitationURL = try XCTUnwrap(c.pendingPairing?.apiBaseURL)
        let target = try XCTUnwrap(c.pairingTargetURL)
        XCTAssertNotEqual(invitationURL, p.apiBaseURL)
        XCTAssertEqual(target, p.apiBaseURL)
        XCTAssertEqual(c.profiles, [p])
        XCTAssertEqual(try f.reopen().profiles, [p])
        XCTAssertNil(c.selectedProfile)
        XCTAssertEqual(try f.store.session(for: p)?.credential, "saved-session")
        let redemptions = await transport.redemptions
        let bindings = await transport.bindings
        XCTAssertTrue(redemptions.isEmpty)
        XCTAssertTrue(bindings.isEmpty)
        consent.approve(target)
        XCTAssertFalse(consent.approved(invitationURL))
        XCTAssertFalse(consent.approved(URL(string: "https://other.example.test/api")!))
        await c.confirmPairing(allowInsecureHTTP: false)
        XCTAssertEqual(c.status, .connected)
        XCTAssertEqual(c.selectedProfile, p)
        XCTAssertTrue(consent.approved(try XCTUnwrap(c.selectedProfile).apiBaseURL))
        XCTAssertNil(c.pairingTargetURL)
    }

    func testAddAndPairAgainReuseSavedNetworkWithoutBindingOldCredentialToTicket() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport()
        let c = coordinator(f, transport: transport)
        let reused = try c.addProfile(name: "Renamed", apiBaseURL: XCTUnwrap(URL(string: "https://A.example.test:443/api/")),
                                      allowInsecureHTTP: false)
        XCTAssertEqual(reused, p)
        XCTAssertEqual(c.profiles, [p])
        XCTAssertEqual(try f.store.session(for: p)?.credential, "saved-session")
        try c.previewPairing("https://a.example.test/login#pair=\(String(repeating: "a", count: 64))")
        await c.confirmPairing(allowInsecureHTTP: false)
        XCTAssertEqual(c.status, .connected)
        XCTAssertEqual(c.selectedProfile, p)
        XCTAssertEqual(c.profiles, [p])
        XCTAssertEqual(try f.store.session(for: p)?.credential, "issued-session")
        let bindings = await transport.bindings
        XCTAssertFalse(bindings.contains(where: { $0?.credential == "saved-session" }))
    }

    func testDeletingInactiveNetworkPreservesActiveIdentityAndClearsLocalSecrets() async throws {
        let f = try fixture()
        let active = try profile(f)
        let other = try profile(f, name: "B")
        try saved(f, profile: active)
        try saved(f, profile: other)
        try f.proofs.save(NativeLoginProof.make(apiBaseURL: other.apiBaseURL))
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(active)
        let identity = c.identity
        let activeContext = c.readingContext
        let otherPartition = try XCTUnwrap(f.store.confirmedReadingContext(for: other)).partition
        var purged: [ReadingPartition] = []
        c.onReadingInvalidation = { purged.append($0) }
        await c.removeProfile(other)
        XCTAssertEqual(c.identity, identity)
        XCTAssertEqual(c.selectedProfile, active)
        XCTAssertEqual(c.readingContext, activeContext)
        XCTAssertEqual(purged, [otherPartition])
        XCTAssertEqual(c.profiles, [active])
        XCTAssertNil(try f.proofs.loadPending(apiBaseURL: other.apiBaseURL))
        XCTAssertNil(try f.vault.read(account: "session-index.\(other.id.uuidString)"))
        XCTAssertEqual(try f.reopen().profiles, [active])
    }

    func testDeletingActiveNetworkDisconnectsAndCannotRestoreDeletedSelection() async throws {
        let f = try fixture()
        let active = try profile(f)
        try saved(f, profile: active)
        let c = coordinator(f, transport: CoordinatorTransport())
        await c.connect(active)
        XCTAssertNotNil(c.identity)
        let partition = try XCTUnwrap(c.readingContext).partition
        var purged: [ReadingPartition] = []
        c.onReadingInvalidation = { purged.append($0) }
        await c.removeProfile(active)
        XCTAssertNil(c.identity)
        XCTAssertNil(c.readingContext)
        XCTAssertEqual(purged, [partition])
        XCTAssertNil(c.selectedProfile)
        XCTAssertEqual(c.status, .disconnected)
        XCTAssertTrue(c.profiles.isEmpty)
        XCTAssertNil(try f.vault.read(account: "session-index.\(active.id.uuidString)"))
        let reopened = try f.reopen()
        XCTAssertNil(reopened.selectedID)
        let restored = ConnectionCoordinator(store: reopened, proofStore: f.proofs, makeTransport: { _ in CoordinatorTransport() })
        await restored.restore()
        XCTAssertEqual(restored.launchState, .connection)
        XCTAssertNil(restored.identity)
    }

    func testDeletingNetworkFencesAnInFlightSessionRestore() async throws {
        let f = try fixture()
        let p = try profile(f)
        try saved(f, profile: p)
        let transport = CoordinatorTransport(gate: .user)
        let c = coordinator(f, transport: transport)
        let connection = Task { await c.connect(p) }
        await transport.waitUntilEntered()
        await c.removeProfile(p)
        await transport.release()
        await connection.value
        XCTAssertTrue(c.profiles.isEmpty)
        XCTAssertNil(c.selectedProfile)
        XCTAssertNil(c.identity)
        XCTAssertEqual(c.status, .disconnected)
        XCTAssertNil(try f.vault.read(account: "session-index.\(p.id.uuidString)"))
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
