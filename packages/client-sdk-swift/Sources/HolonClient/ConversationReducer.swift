import Foundation

/// Published only after an advancing batch checkpoint commits successfully.
public struct HolonConversationCommit: Sendable {
    public let snapshot: HolonConversationSnapshot
    public let detailInvalidations: [String: Int64]
}

/// Value semantics keep visible state and revision tombstones atomic at checkpoints.
public struct HolonConversationReducer: Sendable {
    public private(set) var snapshot: HolonConversationSnapshot
    private let maximumTurns: Int
    private var batch: JSONValue?
    private var mutations: [JSONValue] = []
    private var bytes = 0
    private var removed: [String: Int64] = [:]
    private var through: Int64
    private var detailRevisions: [String: Int64] = [:]

    public init(snapshot: HolonConversationSnapshot, maximumTurns: Int = 180) {
        self.snapshot = snapshot
        self.maximumTurns = maximumTurns
        through = snapshot.raw["snapshot_through_seq"]?.conversationInt ?? -1
    }

    public mutating func accept(_ event: HolonSSEEvent) throws -> HolonConversationSnapshot? {
        try acceptCommit(event)?.snapshot
    }

    public mutating func acceptCommit(_ event: HolonSSEEvent) throws -> HolonConversationCommit? {
        var candidate = self
        do {
            let result = try candidate.apply(event)
            self = candidate
            return result
        } catch {
            // Failed batches cannot be continued; callers must bootstrap/reconnect.
            batch = nil
            mutations.removeAll()
            throw error
        }
    }

    private mutating func apply(_ event: HolonSSEEvent) throws -> HolonConversationCommit? {
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(event.data.utf8))
        guard case .object = raw else { throw HolonConversationError.malformedProtocol }
        let type = raw["type"]?.conversationString ?? event.event
        switch type {
        case "batch_begin":
            guard batch == nil, let id = raw["batch_id"]?.conversationString, !id.isEmpty,
                  let end = raw["through_seq"]?.conversationInt,
                  let start = raw["from_seq"]?.conversationInt, start >= 0, end >= start,
                  raw["runtime_id"] == .string(snapshot.runtimeID),
                  raw["event_log_epoch"] == .string(snapshot.eventLogEpoch),
                  raw["visibility_scope_id"] == .string(snapshot.visibilityScopeID) else {
                throw HolonConversationError.bootstrapRequired
            }
            try conversationVersions(raw)
            if through >= 0 && start > through { throw HolonConversationError.bootstrapRequired }
            batch = raw; bytes = 0; mutations.removeAll()
        case "checkpoint":
            guard let begin = batch,
                  ["batch_id", "through_seq", "event_log_epoch", "visibility_scope_id"].allSatisfy({ begin[$0] == raw[$0] }),
                  let cursor = raw["checkpoint"]?.conversationString, !cursor.isEmpty else {
                throw HolonConversationError.malformedProtocol
            }
            let end = begin["through_seq"]!.conversationInt!
            defer { batch = nil; mutations.removeAll() }
            // Duplicate or out-of-order complete batches cannot roll back a cursor.
            guard end > through else { return nil }
            var turns = snapshot.raw["turns"]!.conversationArray
            var invalidations: [String: Int64] = [:]
            var pending: [String: JSONValue] = [:]
            for input in snapshot.raw["pending_inputs"]!.conversationArray {
                guard let id = input["message_id"]?.conversationString else { throw HolonConversationError.malformedProtocol }
                pending[id] = input
            }
            for change in mutations {
                switch change["type"]?.conversationString {
                case "turn_summary_upsert":
                    guard let turn = change["turn"] else { throw HolonConversationError.malformedProtocol }
                    turns = try conversationTurns(turns + [turn])
                case "detail_invalidated":
                    guard let id = change["turn_id"]?.conversationString, !id.isEmpty,
                          let revision = change["detail_revision"]?.conversationInt, revision >= 0 else {
                        throw HolonConversationError.malformedProtocol
                    }
                    if revision > (detailRevisions[id] ?? -1) {
                        detailRevisions[id] = revision
                        invalidations[id] = revision
                    }
                case "operator_upsert", "operator_remove":
                    let input = change["input"]
                    guard let id = (input?["message_id"] ?? change["message_id"])?.conversationString,
                          let revision = (input?["revision"] ?? change["revision"])?.conversationInt,
                          revision >= 0 else { throw HolonConversationError.malformedProtocol }
                    let previous = pending[id]?["revision"]?.conversationInt ?? -1
                    if revision < previous || revision <= (removed[id] ?? -1) { continue }
                    if let input {
                        if revision == previous && pending[id] != input { throw HolonConversationError.malformedProtocol }
                        pending[id] = input
                    } else { pending.removeValue(forKey: id); removed[id] = revision }
                default: break // Activity frames do not change summaries.
                }
            }
            for turn in turns {
                for input in turn["inputs"]?.conversationArray ?? [] {
                    if let id = input["message_id"]?.conversationString { pending.removeValue(forKey: id) }
                }
            }
            try conversationBound(turns, maximum: maximumTurns)
            guard removed.count <= 4096 else { throw HolonConversationError.bootstrapRequired }
            // Only retained summaries need persistent revision deduplication.
            // Off-window invalidations still reach the client's detail cache.
            let retainedIDs = Set(turns.compactMap { $0["turn_id"]?.conversationString })
            detailRevisions = detailRevisions.filter { retainedIDs.contains($0.key) }
            guard case .object(var fields) = snapshot.raw else { throw HolonConversationError.malformedProtocol }
            fields["turns"] = .array(turns)
            fields["active_turns"] = .array(turns.filter { $0["execution"]?["kind"] == .string("active") })
            fields["pending_inputs"] = .array(pending.keys.sorted().compactMap { pending[$0] })
            fields["snapshot_cursor"] = .string(cursor)
            fields["snapshot_through_seq"] = .integer(end)
            snapshot = try HolonConversationSnapshot(raw: .object(fields)); through = end
            return HolonConversationCommit(snapshot: snapshot, detailInvalidations: invalidations)
        case "reset_required": throw HolonConversationError.bootstrapRequired
        case "operator_upsert", "operator_remove", "turn_summary_upsert", "activity_upsert", "detail_invalidated":
            guard batch != nil else { throw HolonConversationError.malformedProtocol }
            bytes += event.data.utf8.count
            guard bytes <= 4 * 1024 * 1024, mutations.count < 16384 else { throw HolonConversationError.bootstrapRequired }
            if raw["type"] == nil { throw HolonConversationError.malformedProtocol }
            mutations.append(raw)
        default: throw HolonConversationError.malformedProtocol
        }
        return nil
    }
}
