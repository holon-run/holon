import AuthenticationServices
import Foundation
import HolonClient
import Observation

enum ConnectionStatus: String {
    case disconnected, connecting, needsLogin, connected, sessionExpired, incompatible
    case permissionDenied, networkError, storageError, loginFailed, timedOut
}

/// Authentication transport is separate from view lifetime and credential storage.
protocol ConnectionTransport: Sendable {
    func authenticationMode() async throws -> String
    func handshake() async throws -> HolonHandshake
    func currentUser() async throws -> HolonCurrentUser
    func scope() async throws -> (runtimeID: String, visibilityScopeID: String)
    func exchange(_ credential: String, verifier: String?) async throws -> HolonSession
    func redeem(_ ticket: String) async throws -> HolonSession
    func bind(_ session: StoredSession?) async throws -> HolonConnectionIdentity
    func revoke() async throws
    func close() async
}

private actor SDKConnectionTransport: ConnectionTransport {
    let client: HolonClient

    init(profile: ConnectionProfile) throws {
        client = try HolonClient(endpoint: profile.endpoint, networkID: profile.id.uuidString)
    }

    func handshake() async throws -> HolonHandshake { try await client.handshake().value }
    func authenticationMode() async throws -> String {
        let raw = try await client.getJSON(path: ["auth", "method"]).value
        guard case .string(let mode)? = raw["mode"], ["local", "oidc"].contains(mode) else {
            throw HolonClientError.malformedResponse
        }
        return mode
    }
    func currentUser() async throws -> HolonCurrentUser { try await client.currentUser().value }
    func exchange(_ credential: String, verifier: String?) async throws -> HolonSession {
        try await client.exchangeSession(credential: credential, nativeVerifier: verifier).value
    }
    func redeem(_ ticket: String) async throws -> HolonSession {
        try await client.redeemPairingTicket(ticket: ticket).value
    }
    func scope() async throws -> (runtimeID: String, visibilityScopeID: String) {
        let raw = try await client.getJSON(path: ["agents", "snapshot"]).value
        guard case .string(let runtime)? = raw["runtime_id"], !runtime.isEmpty,
              case .string(let visibility)? = raw["visibility_scope_id"], !visibility.isEmpty else {
            throw HolonClientError.malformedResponse
        }
        return (runtime, visibility)
    }
    func bind(_ session: StoredSession?) async throws -> HolonConnectionIdentity {
        try await client.bindIdentity(runtimeID: session?.runtimeID, userID: session?.userID,
                                      visibilityScopeID: session?.visibilityScopeID,
                                      credential: session?.credential.isEmpty == false ? session?.credential : nil)
    }
    func revoke() async throws { try await client.revokeSession() }
    func close() async { await client.close() }
}

@Observable @MainActor
final class ConnectionCoordinator {
    private(set) var status: ConnectionStatus = .disconnected
    private(set) var identity: HolonConnectionIdentity?
    private(set) var supportsOIDC = false
    private(set) var pendingPairing: HolonPairingInvitation?
    private(set) var selectedProfile: ConnectionProfile?
    private(set) var profiles: [ConnectionProfile]
    var isBusy: Bool { status == .connecting }

    @ObservationIgnored private let store: ConnectionStore
    @ObservationIgnored private let proofStore: NativeLoginProofStore
    @ObservationIgnored private let makeTransport: @Sendable (ConnectionProfile) throws -> any ConnectionTransport
    @ObservationIgnored private let deadline: Duration
    @ObservationIgnored private var transport: (any ConnectionTransport)?
    @ObservationIgnored private var epoch = UUID()
    @ObservationIgnored private var timeout: Task<Void, Never>?
    @ObservationIgnored private var browser: NativeBrowserAuthentication?
    @ObservationIgnored private var proof: NativeLoginProof?
    @ObservationIgnored private var exchangingTicket = false

    init(store: ConnectionStore,
         proofStore: NativeLoginProofStore = .init(vault: .init(service: "run.holon.ios.proofs")),
         deadline: Duration = .seconds(30),
         makeTransport: @escaping @Sendable (ConnectionProfile) throws -> any ConnectionTransport = {
             try SDKConnectionTransport(profile: $0)
         }) {
        self.store = store
        self.proofStore = proofStore
        self.deadline = deadline
        self.makeTransport = makeTransport
        profiles = store.profiles
    }

    func addProfile(name: String, apiBaseURL: URL, allowInsecureHTTP: Bool) throws -> ConnectionProfile {
        let profile = try ConnectionProfile(name: name, apiBaseURL: apiBaseURL,
                                            allowInsecureHTTP: allowInsecureHTTP)
        try store.saveProfile(profile)
        profiles = store.profiles
        return profile
    }

    func removeProfile(_ profile: ConnectionProfile) async {
        if selectedProfile?.id == profile.id {
            guard invalidate() else { return }
            selectedProfile = nil
            status = .disconnected
        }
        do {
            if let pending = try proofStore.loadPending(apiBaseURL: profile.apiBaseURL) {
                try proofStore.remove(pending)
            }
            try store.removeProfile(profile.id)
            profiles = store.profiles
        } catch { status = .storageError }
    }

    func restore() async {
        guard selectedProfile == nil,
              let profile = profiles.first(where: { $0.id == store.selectedID }) else { return }
        await connect(profile)
    }

    func refresh() async {
        guard let selectedProfile else { return }
        await connect(selectedProfile)
    }

    func connect(_ profile: ConnectionProfile) async {
        guard invalidate() else { return }
        pendingPairing = nil
        selectedProfile = profile
        let stamp = epoch
        status = .connecting
        do {
            try store.select(profile.id)
            let client = try makeTransport(profile)
            transport = client
            armTimeout(stamp: stamp)
            let saved: StoredSession?
            let pending: PendingCredential?
            do {
                pending = try store.pendingSession(for: profile)
                saved = try store.session(for: profile)
            }
            catch { finish(.storageError); return }
            let candidate = pending.map {
                StoredSession(credential: $0.credential, runtimeID: "", userID: $0.userID,
                              visibilityScopeID: "", expiresAt: $0.expiresAt)
            } ?? saved
            if let candidate {
                _ = try await client.bind(candidate)
                guard current(stamp) else { return }
            }
            let mode = try await client.authenticationMode()
            guard current(stamp) else { return }
            supportsOIDC = mode == "oidc" && profile.apiBaseURL.scheme == "https"
            if candidate == nil, mode == "oidc" {
                finish(.needsLogin)
                do { proof = try proofStore.loadPending(apiBaseURL: profile.apiBaseURL) }
                catch { finish(.storageError) }
                return
            }
            let handshake: HolonHandshake
            do { handshake = try await client.handshake() }
            catch let failure as HolonHTTPFailure where candidate == nil && failure.statusCode == 401 {
                guard current(stamp) else { return }
                finish(.needsLogin)
                return
            }
            guard current(stamp) else { return }
            guard case .compatible = handshake.checkCompatibility() else {
                finish(.incompatible)
                return
            }
            if let candidate {
                try await establish(client, profile: profile, credential: candidate.credential,
                                    expiresAt: candidate.expiresAt,
                                    expectedUser: pending != nil && mode == "oidc" ? pending?.userID : nil,
                                    stamp: stamp)
            } else if handshake.server.authRequired {
                finish(.needsLogin)
            } else {
                try await establish(client, profile: profile, credential: nil,
                                    expiresAt: nil, expectedUser: nil, stamp: stamp)
            }
        } catch { handle(error, stamp: stamp, authenticating: false) }
    }

    func login(token: String) async {
        guard let profile = selectedProfile, let client = transport, !isBusy else { return }
        let stamp = beginAuthentication()
        do {
            let session = try await client.exchange(token, verifier: nil)
            guard current(stamp) else { return }
            try await install(session, client: client, profile: profile, stamp: stamp)
        } catch { handle(error, stamp: stamp, authenticating: true) }
    }

    /// Importing a QR is entirely offline. Confirmation owns the first network request.
    func previewPairing(_ payload: String) throws {
        pendingPairing = try HolonPairingInvitation(payload: payload)
    }

    func cancelPairing() { pendingPairing = nil }

    func confirmPairing(allowInsecureHTTP: Bool) async {
        guard let invitation = pendingPairing else { return }
        do {
            let endpoint = try invitation.endpoint(allowInsecureHTTP: allowInsecureHTTP)
            // Do not bind an existing profile's credential to an imported target.
            let profile = try addProfile(name: endpoint.apiBaseURL.host ?? "Holon",
                                         apiBaseURL: endpoint.apiBaseURL,
                                         allowInsecureHTTP: allowInsecureHTTP)
            guard invalidate() else { return }
            pendingPairing = nil
            selectedProfile = profile
            try store.select(profile.id)
            let stamp = epoch
            let client = try makeTransport(profile)
            transport = client
            status = .connecting
            armTimeout(stamp: stamp)
            do {
                let session = try await client.redeem(invitation.ticket)
                guard current(stamp) else { return }
                try await install(session, client: client, profile: profile, stamp: stamp)
            } catch { handle(error, stamp: stamp, authenticating: true) }
        } catch { finish(.loginFailed) }
    }

    func startOrganizationLogin(anchor: ASPresentationAnchor) {
        guard supportsOIDC, !isBusy, let profile = selectedProfile else { return }
        cancelLogin()
        do {
            let pending = try NativeLoginProof.make(apiBaseURL: profile.apiBaseURL)
            try proofStore.save(pending)
            proof = pending
            let stamp = epoch
            let authentication = NativeBrowserAuthentication(anchor: anchor)
            browser = authentication
            try authentication.prepare(proof: pending) { [weak self] result in
                guard let self, self.current(stamp), self.proof?.state == pending.state else { return }
                switch result {
                case .success(let ticket):
                    Task { await self.exchangeTicket(ticket, proof: pending, stamp: stamp) }
                case .failure:
                    self.cancelLogin()
                    if self.status != .storageError { self.status = .loginFailed }
                }
            }
            status = .connecting
            armTimeout(stamp: stamp, duration: .seconds(NativeLoginProof.lifetime))
            _ = authentication.start()
        } catch {
            cancelLogin()
            if status != .storageError { status = .loginFailed }
        }
    }

    func handleCallback(_ url: URL) async {
        guard let profile = selectedProfile else { return }
        do {
            guard let pending = try proofStore.loadPending(apiBaseURL: profile.apiBaseURL) else { return }
            let ticket = try pending.ticket(from: url)
            proof = pending
            await exchangeTicket(ticket, proof: pending, stamp: epoch)
        } catch {
            // A mismatched callback neither consumes a ticket nor deletes valid proof.
            if !isBusy { status = .loginFailed }
        }
    }

    func cancelLogin() {
        guard proof != nil || browser != nil || isBusy else { return }
        // Advance before cancel(), whose completion may be delivered synchronously.
        epoch = UUID()
        timeout?.cancel()
        timeout = nil
        let previous = transport
        transport = nil
        identity = nil
        Task { await previous?.close() }
        do { try clearProof() }
        catch { status = .storageError; return }
        if let selectedProfile { transport = try? makeTransport(selectedProfile) }
        status = .needsLogin
    }

    /// The system browser can make the app inactive; that is not login cancellation.
    func sceneBecameInactive() {
        if browser == nil { cancelLogin() }
    }

    func validatePendingLogin() {
        if let proof, !proof.isValid(at: Date()) {
            cancelLogin()
            if status != .storageError { status = .timedOut }
        }
    }

    func logout() async {
        guard let profile = selectedProfile else { return }
        let previous = transport
        let cleaned = invalidate(closeTransport: false)
        let stamp = epoch
        status = cleaned ? .needsLogin : .storageError
        do { try store.removeSession(for: profile) }
        catch { status = .storageError }
        // Local deletion is authoritative even when server revocation is unavailable.
        try? await previous?.revoke()
        await previous?.close()
        guard current(stamp), transport == nil else { return }
        transport = try? makeTransport(profile)
    }

    private func exchangeTicket(_ ticket: String, proof: NativeLoginProof, stamp expectedStamp: UUID) async {
        guard current(expectedStamp), !exchangingTicket, let client = transport, let profile = selectedProfile,
              proof.apiBaseURL == profile.apiBaseURL, proof.isValid(at: Date()) else { return }
        exchangingTicket = true
        let stamp = beginAuthentication()
        do {
            let session = try await client.exchange(ticket, verifier: proof.verifier)
            guard current(stamp) else { return }
            // Preserve a confirmed exchange before consuming its recovery proof.
            do {
                try stage(session, profile: profile)
                try clearProof()
            } catch { finish(.storageError); return }
            try await install(session, client: client, profile: profile, stamp: stamp, alreadyStaged: true)
        } catch {
            handle(error, stamp: stamp, authenticating: true)
            if current(stamp) {
                exchangingTicket = false
                if status != .networkError {
                    do { try clearProof() }
                    catch { finish(.storageError) }
                }
            }
        }
    }

    private func clearProof() throws {
        let previous = browser
        browser = nil
        let pending = proof
        proof = nil
        exchangingTicket = false
        previous?.cancel()
        if let pending { try proofStore.remove(pending) }
    }

    private func install(_ session: HolonSession, client: any ConnectionTransport,
                         profile: ConnectionProfile, stamp: UUID, alreadyStaged: Bool = false) async throws {
        guard let credential = session.credential, !credential.isEmpty, session.ok,
              session.expiresAt.map({ $0 > Date() }) ?? true else {
            throw HolonClientError.malformedResponse
        }
        if !alreadyStaged {
            do { try stage(session, profile: profile) }
            catch { finish(.storageError); return }
        }
        let provisional = StoredSession(credential: credential, runtimeID: "", userID: session.userId,
                                        visibilityScopeID: "", expiresAt: session.expiresAt)
        _ = try await client.bind(provisional)
        guard current(stamp) else { return }
        let mode = try await client.authenticationMode()
        guard current(stamp) else { return }
        supportsOIDC = mode == "oidc" && profile.apiBaseURL.scheme == "https"
        let handshake = try await client.handshake()
        guard current(stamp) else { return }
        guard case .compatible = handshake.checkCompatibility() else {
            finish(.incompatible); return
        }
        try await establish(client, profile: profile, credential: credential, expiresAt: session.expiresAt,
                            expectedUser: mode == "oidc" ? session.userId : nil, stamp: stamp)
    }

    private func stage(_ session: HolonSession, profile: ConnectionProfile) throws {
        guard session.ok, let credential = session.credential else { throw HolonClientError.malformedResponse }
        try store.stageSession(.init(apiBaseURL: profile.apiBaseURL, credential: credential,
                                     userID: session.userId, expiresAt: session.expiresAt), for: profile)
    }

    private func establish(_ client: any ConnectionTransport, profile: ConnectionProfile,
                           credential: String?, expiresAt: Date?, expectedUser: String?, stamp: UUID) async throws {
        let user = try await client.currentUser()
        guard current(stamp) else { return }
        guard user.ok, !user.userId.isEmpty, expectedUser == nil || expectedUser == user.userId else {
            throw HolonClientError.malformedResponse
        }
        let scope = try await client.scope()
        guard current(stamp) else { return }
        let context = StoredSession(credential: credential ?? "", runtimeID: scope.runtimeID,
                                    userID: user.userId, visibilityScopeID: scope.visibilityScopeID,
                                    expiresAt: expiresAt)
        let bound = try await client.bind(context)
        guard current(stamp) else { return }
        if credential != nil {
            do { try store.saveSession(context, for: profile) }
            catch { finish(.storageError); return }
        }
        identity = bound
        finish(.connected)
    }

    private func handle(_ error: Error, stamp: UUID, authenticating: Bool) {
        guard current(stamp) else { return }
        if let failure = error as? HolonHTTPFailure {
            if failure.statusCode == 403 { finish(.permissionDenied) }
            else if authenticating && ([408, 429].contains(failure.statusCode) || failure.statusCode >= 500) {
                finish(.networkError)
            }
            else if authenticating { finish(.loginFailed) }
            else if HolonAPIError.requiresSessionRenewal(
                statusCode: failure.statusCode, code: failure.apiError?.code) {
                identity = nil
                do {
                    if let selectedProfile { try store.removeSession(for: selectedProfile) }
                    finish(.sessionExpired)
                } catch { finish(.storageError) }
            } else { finish(.networkError) }
        } else if error is URLError {
            finish(.networkError)
        } else if error is CancellationError || error as? HolonClientError == .staleConnection {
            finish(.disconnected)
        } else { finish(authenticating ? .loginFailed : .networkError) }
    }

    private func current(_ stamp: UUID) -> Bool { epoch == stamp && !Task.isCancelled }

    private func beginAuthentication() -> UUID {
        // Fence prior callbacks and withdraw identity before exchanging credentials.
        epoch = UUID()
        identity = nil
        status = .connecting
        armTimeout(stamp: epoch)
        return epoch
    }

    private func finish(_ state: ConnectionStatus) {
        timeout?.cancel()
        timeout = nil
        status = state
    }

    private func armTimeout(stamp: UUID, duration: Duration? = nil) {
        timeout?.cancel()
        timeout = Task { [weak self, delay = duration ?? deadline] in
            do { try await Task.sleep(for: delay) } catch { return }
            guard let self, self.current(stamp) else { return }
            self.invalidate(preserveProof: self.exchangingTicket)
            if self.status != .storageError { self.status = .timedOut }
        }
    }

    @discardableResult
    private func invalidate(closeTransport: Bool = true, preserveProof: Bool = false) -> Bool {
        epoch = UUID()
        timeout?.cancel()
        timeout = nil
        identity = nil
        supportsOIDC = false
        let previous = transport
        transport = nil
        if closeTransport { Task { await previous?.close() } }
        if preserveProof {
            let previousBrowser = browser
            browser = nil
            exchangingTicket = false
            previousBrowser?.cancel()
            return true
        }
        do { try clearProof(); return true }
        catch { status = .storageError; return false }
    }
}
