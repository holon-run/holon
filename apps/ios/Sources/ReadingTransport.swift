import Foundation
import HolonClient

struct ReadingStream: Sendable {
    let events: AsyncThrowingStream<HolonSSEEvent, any Error>
    let close: @Sendable () -> Void
}

protocol ReadingTransport: Sendable {
    func roster() async throws -> [ReadingAgent]
    func operatorPreview(agentID: String) async throws -> ReadingOperatorPreview?
    func conversation(agentID: String, before: String?) async throws -> HolonConversationSnapshot
    func brief(agentID: String, briefID: String) async throws -> JSONValue
    func activities(agentID: String, turnID: String) async throws -> JSONValue
    func activities(agentID: String, turnID: String, before: String?) async throws -> JSONValue
    func activityDetail(agentID: String, turnID: String, activity: ReadingActivity) async throws -> JSONValue
    func markRead(agentID: String, through: Int64) async throws -> JSONValue
    func stream(agentID: String?, after: String?) async throws -> ReadingStream
    func close() async
}

extension ReadingTransport {
    func operatorPreview(agentID: String) async throws -> ReadingOperatorPreview? { nil }
    func activities(agentID: String, turnID: String, before: String?) async throws -> JSONValue {
        guard before == nil else { throw HolonClientError.invalidRequest }
        return try await activities(agentID: agentID, turnID: turnID)
    }
    func activityDetail(agentID: String, turnID: String, activity: ReadingActivity) async throws -> JSONValue {
        throw HolonClientError.invalidRequest
    }
}

/// SDK identity is independently bound; original connection identity remains the UI authority.
actor ReadingClientTransport: ReadingTransport {
    private let client: HolonClient
    private let authority: HolonConnectionIdentity
    private var bound: HolonConnectionIdentity?
    private var rosterEpoch: String?
    private var rosterGeneration: UInt64 = 0
    private var rosterLoading = false

    init(client: HolonClient, authority: HolonConnectionIdentity) {
        self.client = client
        self.authority = authority
    }

    private func expected() async throws -> HolonConnectionIdentity {
        let current = await client.identity
        guard let runtime = authority.runtimeID, !runtime.isEmpty,
              let user = authority.userID, !user.isEmpty,
              let visibility = authority.visibilityScopeID, !visibility.isEmpty,
              current.networkID == authority.networkID,
              current.runtimeID == authority.runtimeID,
              current.userID == authority.userID,
              current.visibilityScopeID == authority.visibilityScopeID,
              bound == nil || bound == current else { throw CancellationError() }
        bound = current
        return current
    }

    private func request<T: Sendable>(
        _ operation: @Sendable (HolonClient) async throws -> HolonResponse<T>
    ) async throws -> T {
        let identity = try await expected()
        do {
            let response = try await operation(client)
            guard response.identity == identity, try await expected() == identity else {
                throw CancellationError()
            }
            return response.value
        } catch let error as HolonHTTPFailure {
            guard error.identity == identity, try await expected() == identity else {
                throw CancellationError()
            }
            throw HolonHTTPFailure(statusCode: error.statusCode, apiError: error.apiError, identity: authority)
        }
    }

    func roster() async throws -> [ReadingAgent] {
        guard !rosterLoading else { throw CancellationError() }
        rosterLoading = true
        defer { rosterLoading = false }
        rosterGeneration &+= 1
        let generation = rosterGeneration
        let raw = try await request { try await $0.getJSON(path: ["agents", "snapshot"]) }
        guard generation == rosterGeneration else { throw CancellationError() }
        guard raw["runtime_id"]?.readingString == authority.runtimeID,
              raw["visibility_scope_id"]?.readingString == authority.visibilityScopeID,
              let epoch = raw["event_log_epoch"]?.readingString, !epoch.isEmpty,
              case .array(let entries) = raw["agents"],
              (try JSONEncoder().encode(raw)).count <= 4_194_304 else {
            throw HolonConversationError.bootstrapRequired
        }
        rosterEpoch = epoch
        let agents = try entries.map { entry in
            guard let agent = entry["agent"]?["identity"], let id = agent["agent_id"]?.readingString,
                  !id.isEmpty, id.utf8.count <= 512 else {
                throw HolonConversationError.malformedProtocol
            }
            return ReadingAgent(id: id, name: agent["name"]?.readingString ?? id,
                                preview: String((entry["latest_brief"]?["preview"]?.readingString ?? "").prefix(240)),
                                currentRunID: entry["agent"]?["current_run_id"]?.readingString,
                                posture: entry["agent"]?["scheduling_posture"]?["posture"]?.readingString,
                                briefAt: entry["latest_brief"]?["created_at"]?.readingString.flatMap(Self.metadataDate),
                                effectiveModel: entry["agent"]?["model"]?["effective_model"]?.readingString)
        }
        var counts: [String: Int] = [:]
        do {
            let states = try await request { try await $0.getJSON(path: ["agents", "brief-read-states"]) }
            counts = try Self.unreadCounts(states, agentIDs: Set(agents.map(\.id)),
                                           epoch: epoch, visibility: authority.visibilityScopeID!)
        } catch { try Self.requireMetadataRecovery(error) }
        let enriched = agents.map {
                ReadingAgent(id: $0.id, name: $0.name, preview: $0.preview,
                             unreadCount: counts[$0.id], currentRunID: $0.currentRunID,
                             posture: $0.posture, briefAt: $0.briefAt, effectiveModel: $0.effectiveModel)
        }
        _ = try await expected()
        guard generation == rosterGeneration else { throw CancellationError() }
        return enriched
    }

    private static func requireMetadataRecovery(_ error: any Error) throws {
        if error is CancellationError { throw error }
        if let failure = error as? HolonHTTPFailure,
           failure.statusCode == 401 || failure.statusCode == 403 { throw error }
        if let failure = error as? HolonConversationError, failure == .bootstrapRequired { throw error }
    }

    static func unreadCounts(_ raw: JSONValue, agentIDs: Set<String>,
                             epoch: String, visibility: String) throws -> [String: Int] {
        guard case .array(let states) = raw else { return [:] }
        var counts: [String: Int] = [:]
        var seen: Set<String> = []
        for state in states {
            guard let id = state["agent_id"]?.readingString, agentIDs.contains(id) else { continue }
            if let value = state["event_log_epoch"]?.readingString, value != epoch {
                throw HolonConversationError.bootstrapRequired
            }
            guard seen.insert(id).inserted else { counts[id] = nil; continue }
            guard state["event_log_epoch"]?.readingString == epoch,
                  state["visibility_scope_id"]?.readingString == visibility,
                  state["reset_required"] == .bool(false),
                  state["retention_gap"] == .bool(false),
                  let count = state["unread_count"]?.readingInteger, count >= 0,
                  let exact = Int(exactly: count) else { continue }
            counts[id] = exact
        }
        return counts
    }

    func operatorPreview(agentID: String) async throws -> ReadingOperatorPreview? {
        guard let epoch = rosterEpoch else { throw HolonConversationError.bootstrapRequired }
        do {
            let snapshot = try await request { try await $0.conversation(agentID: agentID, limit: 1) }
            guard snapshot.eventLogEpoch == epoch, snapshot.agentID == agentID,
                  snapshot.runtimeID == authority.runtimeID,
                  snapshot.visibilityScopeID == authority.visibilityScopeID else {
                throw HolonConversationError.bootstrapRequired
            }
            return Self.operatorPreviewWithDate(snapshot.raw)
        } catch {
            if let failure = error as? HolonClientError, failure == .malformedResponse {
                throw HolonConversationError.bootstrapRequired
            }
            try Self.requireMetadataRecovery(error)
            return nil
        }
    }

    static func operatorPreview(_ raw: JSONValue) -> String? {
        operatorPreviewWithDate(raw)?.text
    }

    static func operatorPreviewWithDate(_ raw: JSONValue) -> ReadingOperatorPreview? {
        var candidates: [(Date, Int, String)] = []
        func append(_ input: JSONValue, fallback: String?, startedAt: String? = nil) {
            guard (input["presentation_class"]?.readingString ?? fallback) == "operator",
                  let timestamp = input["created_at"]?.readingString ?? startedAt,
                  let date = metadataDate(timestamp),
                  let preview = input["preview"]?.readingString else { return }
            var text = preview
            if let envelope = try? JSONDecoder().decode(JSONValue.self, from: Data(preview.utf8)),
               envelope["type"]?.readingString == "text",
               let value = envelope["text"]?.readingString { text = value }
            guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
            candidates.append((date, candidates.count, String(text.prefix(240))))
        }
        for key in ["turns", "active_turns"] {
            if case .array(let turns) = raw[key] {
                for turn in turns {
                    if case .array(let ids) = turn["brief_ids"], !ids.isEmpty { continue }
                    if case .array(let inputs) = turn["inputs"] {
                        for input in inputs {
                            append(input, fallback: turn["presentation_class"]?.readingString,
                                   startedAt: turn["started_at"]?.readingString)
                        }
                    }
                }
            }
        }
        if case .array(let inputs) = raw["pending_inputs"] {
            for input in inputs { append(input, fallback: nil) }
        }
        return candidates.max { $0.0 == $1.0 ? $0.1 < $1.1 : $0.0 < $1.0 }
            .map { ReadingOperatorPreview(text: $0.2, createdAt: $0.0) }
    }

    private static func metadataDate(_ value: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: value) ?? ISO8601DateFormatter().date(from: value)
    }

    func conversation(agentID: String, before: String?) async throws -> HolonConversationSnapshot {
        let value = try await request { try await $0.conversation(agentID: agentID, before: before, limit: 60) }
        guard value.eventLogEpoch == rosterEpoch else { throw HolonConversationError.bootstrapRequired }
        return value
    }
    func brief(agentID: String, briefID: String) async throws -> JSONValue {
        try await request { try await $0.briefDetail(agentID: agentID, briefID: briefID) }
    }
    func activities(agentID: String, turnID: String) async throws -> JSONValue {
        try await activities(agentID: agentID, turnID: turnID, before: nil)
    }
    func activities(agentID: String, turnID: String, before: String?) async throws -> JSONValue {
        try await request { try await $0.conversationActivities(agentID: agentID, turnID: turnID, before: before, limit: 60) }
    }
    func activityDetail(agentID: String, turnID: String, activity: ReadingActivity) async throws -> JSONValue {
        guard let id = activity.detailID else { throw HolonClientError.invalidRequest }
        let segment = activity.kind == "tool" ? "tool-executions" : "transcript"
        let raw = try await request { try await $0.getJSON(path: ["agents", agentID, segment, id]) }
        guard raw["id"] == .string(id), raw["agent_id"] == .string(agentID),
              (try JSONEncoder().encode(raw)).count <= 1_048_576 else { throw HolonClientError.malformedResponse }
        if activity.kind == "tool" {
            guard raw["turn_id"] == nil || raw["turn_id"] == .null || raw["turn_id"] == .string(turnID) else {
                throw HolonClientError.malformedResponse
            }
        } else {
            guard raw["kind"] == .string("assistant_round") || raw["kind"] == .string("subagent_assistant_round"),
                  raw["data"]?["turn_id"] == nil || raw["data"]?["turn_id"] == .string(turnID) else {
                throw HolonClientError.malformedResponse
            }
        }
        return raw
    }
    func markRead(agentID: String, through: Int64) async throws -> JSONValue {
        try await request { try await $0.markBriefRead(agentID: agentID, readThroughEventSeq: through) }
    }
    func stream(agentID: String?, after: String?) async throws -> ReadingStream {
        let identity = try await expected()
        let path = agentID.map { ["agents", $0, "conversation", "stream"] } ?? ["events", "stream"]
        let opened: HolonEventStream
        do {
            opened = try await client.openEventStream(path: path, query: after.map { ["after": $0] } ?? [:])
        } catch let error as HolonHTTPFailure {
            guard error.identity == identity, try await expected() == identity else { throw CancellationError() }
            throw HolonHTTPFailure(statusCode: error.statusCode, apiError: error.apiError, identity: authority)
        }
        guard try await expected() == identity else { opened.close(); throw CancellationError() }
        let (events, continuation) = AsyncThrowingStream<HolonSSEEvent, any Error>.makeStream(
            bufferingPolicy: .bufferingOldest(64))
        let worker = Task {
            do {
                for try await response in opened {
                    guard response.identity == identity, try await self.expected() == identity else {
                        throw CancellationError()
                    }
                    if case .dropped = continuation.yield(response.value) {
                        throw HolonConversationError.bootstrapRequired
                    }
                }
                continuation.finish()
            } catch { continuation.finish(throwing: error) }
            opened.close()
        }
        continuation.onTermination = { _ in worker.cancel(); opened.close() }
        return ReadingStream(events: events, close: { worker.cancel(); opened.close() })
    }
    func close() async { await client.close() }
}
