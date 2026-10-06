import Foundation

/// A foreground transport generation, not an authentication or sync coordinator.
public actor HolonClient {
    public let endpoint: HolonEndpoint
    public private(set) var identity: HolonConnectionIdentity
    private var credential: String?
    private let configuration: URLSessionConfiguration
    private let maximumResponseBytes: Int
    private var session: URLSession
    private var streamSessions: [UUID: URLSession] = [:]
    private var closed = false

    public init(endpoint: HolonEndpoint, networkID: String, credential: String? = nil,
                configuration: URLSessionConfiguration = .ephemeral,
                maximumResponseBytes: Int = 16_777_216) throws {
        guard maximumResponseBytes > 0,
              credential?.contains(where: { $0 == "\r" || $0 == "\n" }) != true else {
            throw HolonClientError.invalidRequest
        }
        self.endpoint = endpoint
        identity = HolonConnectionIdentity(networkID: networkID)
        self.credential = credential
        // No shared cookies, disk cache, credential storage, or redirects.
        let config = configuration.copy() as! URLSessionConfiguration
        config.httpCookieStorage = nil
        config.httpShouldSetCookies = false
        config.urlCache = nil
        config.urlCredentialStorage = nil
        self.configuration = config
        self.maximumResponseBytes = maximumResponseBytes
        session = Self.makeSession(config)
    }

    /// Cancels every old request/stream and advances the identity generation.
    @discardableResult
    public func bindIdentity(runtimeID: String?, userID: String?, visibilityScopeID: String?,
                             credential: String?) throws -> HolonConnectionIdentity {
        guard !closed else { throw HolonClientError.closed }
        guard credential?.contains(where: { $0 == "\r" || $0 == "\n" }) != true else {
            throw HolonClientError.invalidRequest
        }
        session.invalidateAndCancel()
        for stream in streamSessions.values { stream.invalidateAndCancel() }
        streamSessions.removeAll()
        identity = HolonConnectionIdentity(networkID: identity.networkID, runtimeID: runtimeID,
                                           userID: userID, visibilityScopeID: visibilityScopeID)
        self.credential = credential
        session = Self.makeSession(configuration)
        return identity
    }

    public func close() {
        closed = true
        session.invalidateAndCancel()
        for stream in streamSessions.values { stream.invalidateAndCancel() }
        streamSessions.removeAll()
        credential = nil
    }

    deinit {
        session.invalidateAndCancel()
        for stream in streamSessions.values { stream.invalidateAndCancel() }
    }

    public func handshake(retry: HolonRetryPolicy = .none,
                          onRetry: (@Sendable (HolonRetryNotice) -> Void)? = nil)
        async throws -> HolonResponse<HolonHandshake> {
        try await read(path: ["handshake"], retry: retry, onRetry: onRetry, decode: HolonHandshake.init)
    }

    public func listAgents(retry: HolonRetryPolicy = .none)
        async throws -> HolonResponse<HolonRoster> {
        try await read(path: ["agents", "list"], retry: retry, decode: HolonRoster.init)
    }

    public func currentUser() async throws -> HolonResponse<HolonCurrentUser> {
        try await read(path: ["auth", "session", "me"], decode: HolonCurrentUser.init)
    }

    /// Returns credentials to the caller; never persists or implicitly installs them.
    public func exchangeSession(credential: String, nativeVerifier: String? = nil)
        async throws -> HolonResponse<HolonSession> {
        guard !credential.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw HolonClientError.invalidRequest
        }
        var body: [String: JSONValue] = ["credential": .string(credential)]
        if let nativeVerifier { body["native_verifier"] = .string(nativeVerifier) }
        let result = try await request(path: ["auth", "session", "exchange", "native"],
                                       method: "POST", body: .object(body), authenticated: false)
        let response = try decoded(result, using: HolonSession.init)
        guard response.value.ok,
              let returnedCredential = response.value.credential,
              !returnedCredential.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw HolonClientError.malformedResponse
        }
        return response
    }

    /// Redeems once without sending or installing an existing session credential.
    public func redeemPairingTicket(ticket: String) async throws -> HolonResponse<HolonSession> {
        guard ticket.utf8.count == 64,
              ticket.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) ||
                  (97...102).contains($0) }) else {
            throw HolonClientError.invalidRequest
        }
        let result = try await request(path: ["auth", "pairing", "redeem", "native"],
                                       method: "POST", body: .object(["ticket": .string(ticket)]),
                                       authenticated: false)
        let response = try decoded(result, using: HolonSession.init)
        guard response.value.ok,
              let credential = response.value.credential,
              !credential.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw HolonClientError.malformedResponse
        }
        return response
    }

    /// Permission and transport failures never clear the credential.
    public func revokeSession() async throws {
        _ = try await request(path: ["auth", "session", "logout"], method: "POST")
    }

    public func getJSON(path: [String], query: [String: String] = [:],
                        retry: HolonRetryPolicy = .none)
        async throws -> HolonResponse<JSONValue> {
        try await read(path: path, query: query, retry: retry) {
            try JSONDecoder().decode(JSONValue.self, from: $0)
        }
    }

    private func read<T: Sendable>(
        path: [String], query: [String: String] = [:], retry: HolonRetryPolicy = .none,
        onRetry: (@Sendable (HolonRetryNotice) -> Void)? = nil,
        decode: @Sendable (Data) throws -> T
    ) async throws -> HolonResponse<T> {
        let result = try await request(path: path, query: query, retry: retry, onRetry: onRetry)
        return try decoded(result, using: decode)
    }

    private func decoded<T: Sendable>(_ result: HolonResponse<Data>,
                                      using decode: (Data) throws -> T) throws -> HolonResponse<T> {
        do { return HolonResponse(identity: result.identity, value: try decode(result.value)) }
        catch { throw HolonClientError.malformedResponse }
    }

    private func request(path: [String], query: [String: String] = [:],
                         method: String = "GET", body: JSONValue? = nil,
                         authenticated: Bool = true, retry: HolonRetryPolicy = .none,
                         onRetry: (@Sendable (HolonRetryNotice) -> Void)? = nil)
        async throws -> HolonResponse<Data> {
        let captured = identity
        let request = try makeRequest(path: path, query: query, method: method, body: body,
                                      authenticated: authenticated)
        let transport = session
        for attempt in 1...retry.maxAttempts {
            try check(captured)
            do {
                let (bytes, response) = try await transport.bytes(for: request)
                let data = try await collect(bytes)
                try check(captured)
                try validate(response: response, data: data, identity: captured, streaming: false)
                return HolonResponse(identity: captured, value: data)
            } catch {
                try check(captured)
                guard method == "GET", attempt < retry.maxAttempts,
                      Self.isTransient(error) else { throw error }
                try await pause(retry, after: attempt, identity: captured, onRetry: onRetry)
            }
        }
        throw HolonClientError.invalidRetryPolicy
    }

    /// Retries connection establishment only. EOF/replay recovery belongs to the caller.
    public func openEventStream(
        path: [String], query: [String: String] = [:], lastEventID: String? = nil,
        retry: HolonRetryPolicy = .none, maximumFrameBytes: Int = 1_048_576,
        bufferCapacity: Int = 64, onRetry: (@Sendable (HolonRetryNotice) -> Void)? = nil
    ) async throws -> HolonEventStream {
        guard bufferCapacity > 0, maximumFrameBytes > 0,
              lastEventID?.contains(where: { $0 == "\r" || $0 == "\n" || $0 == "\0" }) != true else {
            throw HolonClientError.invalidRequest
        }
        let captured = identity
        var request = try makeRequest(path: path, query: query)
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        if let lastEventID { request.setValue(lastEventID, forHTTPHeaderField: "Last-Event-ID") }
        let streamID = UUID()
        let transport = Self.makeSession(configuration)
        streamSessions[streamID] = transport
        do {
            for attempt in 1...retry.maxAttempts {
                try check(captured)
                do {
                    let (bytes, response) = try await transport.bytes(for: request)
                    do {
                        try check(captured)
                        if let http = response as? HTTPURLResponse, !(200...299).contains(http.statusCode) {
                            let data = try await collect(bytes)
                            try validate(response: response, data: data, identity: captured, streaming: true)
                        }
                        try validate(response: response, data: Data(), identity: captured, streaming: true)
                    } catch {
                        bytes.task.cancel()
                        throw error
                    }
                    return makeStream(bytes: bytes, transport: transport, streamID: streamID,
                                      identity: captured, maximumFrameBytes: maximumFrameBytes,
                                      bufferCapacity: bufferCapacity)
                } catch {
                    try check(captured)
                    guard attempt < retry.maxAttempts, Self.isTransient(error) else { throw error }
                    try await pause(retry, after: attempt, identity: captured, onRetry: onRetry)
                }
            }
            throw HolonClientError.invalidRetryPolicy
        } catch {
            transport.invalidateAndCancel()
            streamSessions.removeValue(forKey: streamID)
            throw error
        }
    }

    private func makeStream(bytes: URLSession.AsyncBytes, transport: URLSession, streamID: UUID,
                            identity captured: HolonConnectionIdentity, maximumFrameBytes: Int,
                            bufferCapacity: Int) -> HolonEventStream {
        let (events, continuation) = AsyncThrowingStream<HolonResponse<HolonSSEEvent>, any Error>
            .makeStream(bufferingPolicy: .bufferingOldest(bufferCapacity))
        let worker = Task { [weak self] in
            defer { transport.invalidateAndCancel() }
            do {
                var parser = try HolonSSEParser(maximumFrameBytes: maximumFrameBytes)
                for try await byte in bytes {
                    try Task.checkCancellation()
                    guard let self else { throw HolonClientError.closed }
                    try await self.check(captured)
                    if let event = try parser.append(byte) {
                        switch continuation.yield(HolonResponse(identity: captured, value: event)) {
                        case .dropped: throw HolonClientError.streamLimitExceeded
                        case .terminated: throw CancellationError()
                        case .enqueued: break
                        @unknown default: throw HolonClientError.streamLimitExceeded
                        }
                    }
                }
                guard let self else { throw HolonClientError.closed }
                try Task.checkCancellation()
                try await self.check(captured)
                if let event = parser.finish() {
                    switch continuation.yield(HolonResponse(identity: captured, value: event)) {
                    case .dropped: throw HolonClientError.streamLimitExceeded
                    case .terminated: throw CancellationError()
                    case .enqueued: break
                    @unknown default: throw HolonClientError.streamLimitExceeded
                    }
                }
                throw HolonClientError.streamEnded
            } catch { continuation.finish(throwing: error) }
            await self?.removeStream(streamID)
        }
        continuation.onTermination = { _ in worker.cancel(); transport.invalidateAndCancel() }
        return HolonEventStream(events: events) {
            worker.cancel()
            transport.invalidateAndCancel()
            continuation.finish(throwing: CancellationError())
        }
    }

    private func removeStream(_ id: UUID) { streamSessions.removeValue(forKey: id) }

    private func collect(_ bytes: URLSession.AsyncBytes) async throws -> Data {
        defer { bytes.task.cancel() }
        var result = Data()
        for try await byte in bytes {
            try Task.checkCancellation()
            guard result.count < maximumResponseBytes else { throw HolonClientError.streamLimitExceeded }
            result.append(byte)
        }
        return result
    }

    private func makeRequest(path: [String], query: [String: String] = [:],
                             method: String = "GET", body: JSONValue? = nil,
                             authenticated: Bool = true) throws -> URLRequest {
        guard !closed else { throw HolonClientError.closed }
        var request = URLRequest(url: try endpoint.url(path: path, query: query))
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if authenticated, let credential {
            request.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        }
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONEncoder().encode(body)
        }
        return request
    }

    private func validate(response: URLResponse, data: Data, identity: HolonConnectionIdentity,
                          streaming: Bool) throws {
        guard let http = response as? HTTPURLResponse else { throw HolonClientError.malformedResponse }
        guard (200...299).contains(http.statusCode) else {
            throw HolonHTTPFailure(statusCode: http.statusCode, apiError: try? HolonAPIError(data: data),
                                   identity: identity)
        }
        let mime = http.value(forHTTPHeaderField: "Content-Type")?
            .split(separator: ";", maxSplits: 1).first?
            .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let valid = streaming ? mime == "text/event-stream" :
            mime == "application/json" || mime?.hasSuffix("+json") == true || http.statusCode == 204
        guard valid else { throw HolonClientError.unexpectedContentType }
    }

    private func check(_ captured: HolonConnectionIdentity) throws {
        guard captured == identity else { throw HolonClientError.staleConnection }
        guard !closed else { throw HolonClientError.closed }
        try Task.checkCancellation()
    }

    private func pause(_ policy: HolonRetryPolicy, after attempt: Int,
                       identity: HolonConnectionIdentity,
                       onRetry: (@Sendable (HolonRetryNotice) -> Void)?) async throws {
        let delay = policy.delay(afterAttempt: attempt)
        onRetry?(HolonRetryNotice(identity: identity, nextAttempt: attempt + 1, delay: delay))
        try await Task.sleep(for: .seconds(delay))
        try check(identity)
    }

    private static func isTransient(_ error: any Error) -> Bool {
        if let http = error as? HolonHTTPFailure {
            return [408, 425, 429].contains(http.statusCode) || (500...599).contains(http.statusCode)
        }
        guard let url = error as? URLError else { return false }
        return [.timedOut, .networkConnectionLost, .cannotConnectToHost, .cannotFindHost,
                .dnsLookupFailed, .notConnectedToInternet, .resourceUnavailable].contains(url.code)
    }

    private static func makeSession(_ configuration: URLSessionConfiguration) -> URLSession {
        URLSession(configuration: configuration, delegate: RejectRedirects(), delegateQueue: nil)
    }
}

/// Redirects are observable failures, never a credential or prefix transfer.
private final class RejectRedirects: NSObject, URLSessionTaskDelegate, Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping @Sendable (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
