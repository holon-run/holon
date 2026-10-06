import Foundation

public enum HolonConversationError: Error, Equatable, Sendable {
    case malformedProtocol
    case bootstrapRequired
}

extension JSONValue {
    var conversationString: String? { if case .string(let v) = self { return v }; return nil }
    var conversationInt: Int64? { if case .integer(let v) = self { return v }; return nil }
    var conversationArray: [JSONValue] { if case .array(let v) = self { return v }; return [] }
}

func conversationVersions(_ raw: JSONValue) throws {
    // Snapshots and batch_begin require both versions; patch frames do not.
    for key in ["schema_version", "query_version"] {
        guard let version = raw[key]?.conversationInt, (1...2).contains(version) else {
            throw HolonConversationError.malformedProtocol
        }
    }
}

func conversationTurns(_ values: [JSONValue]) throws -> [JSONValue] {
    var entities: [String: JSONValue] = [:]
    for turn in values {
        guard let id = turn["turn_id"]?.conversationString, !id.isEmpty,
              let index = turn["key"]?["turn_index"]?.conversationInt, index >= 0,
              turn["key"]?["turn_id"]?.conversationString == id,
              let revision = turn["revision"]?.conversationInt, revision >= 0 else {
            throw HolonConversationError.malformedProtocol
        }
        if let previous = entities[id] {
            if previous["execution"]?["kind"] == .string("terminal"),
               turn["execution"]?["kind"] == .string("active") { continue }
            let old = previous["revision"]!.conversationInt!
            if revision < old { continue }
            if revision == old && previous != turn { throw HolonConversationError.malformedProtocol }
        }
        entities[id] = turn
    }
    return entities.values.sorted {
        let a = $0["key"]!["turn_index"]!.conversationInt!
        let b = $1["key"]!["turn_index"]!.conversationInt!
        return a == b ? $0["turn_id"]!.conversationString! < $1["turn_id"]!.conversationString! : a < b
    }
}

public struct HolonConversationSnapshot: Equatable, Sendable {
    public let raw: JSONValue
    public let runtimeID: String
    public let eventLogEpoch: String
    public let agentID: String
    public let visibilityScopeID: String
    public let snapshotCursor: String
    public let hasMore: Bool
    public let nextBeforeCursor: String?

    public init(raw: JSONValue) throws {
        guard case .object(var fields) = raw else { throw HolonConversationError.malformedProtocol }
        try conversationVersions(raw)
        func required(_ key: String) throws -> String {
            guard let value = raw[key]?.conversationString, !value.isEmpty else {
                throw HolonConversationError.malformedProtocol
            }
            return value
        }
        runtimeID = try required("runtime_id")
        eventLogEpoch = try required("event_log_epoch")
        agentID = try required("agent_id")
        visibilityScopeID = try required("visibility_scope_id")
        snapshotCursor = try required("snapshot_cursor")
        guard case .bool(let more) = raw["has_more"] else { throw HolonConversationError.malformedProtocol }
        hasMore = more
        nextBeforeCursor = raw["next_before_cursor"]?.conversationString
        guard !more || nextBeforeCursor != nil else { throw HolonConversationError.malformedProtocol }
        for key in ["turns", "active_turns", "pending_inputs"] {
            guard case .array = raw[key] else { throw HolonConversationError.malformedProtocol }
        }
        let turns = try conversationTurns(raw["turns"]!.conversationArray + raw["active_turns"]!.conversationArray)
        fields["turns"] = .array(turns)
        fields["active_turns"] = .array(turns.filter { $0["execution"]?["kind"] == .string("active") })
        self.raw = .object(fields)
    }
}

func conversationBound(_ turns: [JSONValue], maximum: Int) throws {
    guard maximum > 0 else { throw HolonConversationError.malformedProtocol }
    guard turns.filter({ $0["execution"]?["kind"] != .string("active") }).count <= maximum else {
        throw HolonConversationError.bootstrapRequired
    }
}

public func mergeConversationHistory(_ existing: HolonConversationSnapshot,
                                     page: HolonConversationSnapshot,
                                     maximumTurns: Int = 180) throws -> HolonConversationSnapshot {
    guard existing.runtimeID == page.runtimeID, existing.eventLogEpoch == page.eventLogEpoch,
          existing.agentID == page.agentID, existing.visibilityScopeID == page.visibilityScopeID else {
        throw HolonConversationError.bootstrapRequired
    }
    let turns = try conversationTurns(existing.raw["turns"]!.conversationArray + page.raw["turns"]!.conversationArray)
    try conversationBound(turns, maximum: maximumTurns)
    guard case .object(var fields) = existing.raw else { throw HolonConversationError.malformedProtocol }
    fields["turns"] = .array(turns)
    fields["active_turns"] = .array(turns.filter { $0["execution"]?["kind"] == .string("active") })
    fields["has_more"] = .bool(page.hasMore)
    fields["next_before_cursor"] = page.nextBeforeCursor.map(JSONValue.string) ?? .null
    return try HolonConversationSnapshot(raw: .object(fields))
}
