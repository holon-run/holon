import Foundation
import HolonClient

struct ReadingAgent: Identifiable, Equatable, Codable, Sendable {
    let id: String
    var name: String
    var preview: String
    var operatorPreview: String?
    let unreadCount: Int?
    var currentRunID: String?
    var posture: String?
    var briefAt: Date?
    var operatorAt: Date?
    var effectiveModel: String?

    init(id: String, name: String, preview: String,
         operatorPreview: String? = nil, unreadCount: Int? = nil,
         currentRunID: String? = nil, posture: String? = nil,
         briefAt: Date? = nil, operatorAt: Date? = nil, effectiveModel: String? = nil) {
        self.id = id
        self.name = name
        self.preview = preview
        self.operatorPreview = operatorPreview
        self.unreadCount = unreadCount
        self.currentRunID = currentRunID
        self.posture = posture
        self.briefAt = briefAt
        self.operatorAt = operatorAt
        self.effectiveModel = effectiveModel
    }
}

struct ReadingOperatorPreview: Equatable, Sendable {
    let text: String
    let createdAt: Date
}

enum OperatorInputPresentation {
    static func text(_ preview: String) -> String {
        if let envelope = try? JSONDecoder().decode(JSONValue.self, from: Data(preview.utf8)),
           envelope["type"]?.readingString == "text",
           let text = envelope["text"]?.readingString { return text }

        let prefix = #"{"type":"text","text":""#
        guard preview.hasPrefix(prefix) else { return preview }
        let encoded = preview.dropFirst(prefix.count)
        // A byte cut can leave a closing quote, an escape, or half a surrogate pair.
        for trim in 0...min(11, encoded.count) {
            if trim > 0, !isIncompleteTail(encoded.suffix(trim)) { continue }
            let candidate = "\"" + encoded.dropLast(trim) + "\""
            if let text = try? JSONDecoder().decode(String.self, from: Data(candidate.utf8)) {
                return text + "…"
            }
        }
        return preview
    }

    private static func isIncompleteTail(_ tail: Substring) -> Bool {
        if tail == "\"" || tail == "\\" { return true }
        guard tail.hasPrefix("\\u") else { return false }
        let units = tail.dropFirst(2)
        if units.count < 4 { return isHex(units) }
        guard let high = UInt16(units.prefix(4), radix: 16),
              (0xD800...0xDBFF).contains(high) else { return false }
        let rest = units.dropFirst(4)
        if rest.isEmpty || rest == "\\" { return true }
        guard rest.hasPrefix("\\u") else { return false }
        let low = rest.dropFirst(2)
        return low.count < 4 && isHex(low)
    }

    private static func isHex(_ text: Substring) -> Bool {
        text.utf8.allSatisfy { (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }
    }
}

enum AgentListFilter: String, CaseIterable { case all, reply, newResults, active }

enum AgentSummaryPresentation {
    static func needsReply(_ agent: ReadingAgent) -> Bool { agent.posture == "waiting_for_operator" }
    static func activityDate(_ agent: ReadingAgent) -> Date? {
        [agent.briefAt, agent.operatorAt].compactMap { $0 }.max()
    }
    static func preview(_ agent: ReadingAgent) -> String {
        if showsOperatorPreview(agent), let input = agent.operatorPreview { return input }
        return plainBriefPreview(agent.preview)
    }
    static func showsOperatorPreview(_ agent: ReadingAgent) -> Bool {
        guard let input = agent.operatorPreview, !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
        return agent.briefAt == nil || agent.operatorAt.map { $0 > agent.briefAt! } == true
    }
    static func plainBriefPreview(_ text: String) -> String {
        let bounded = String(text.prefix(2_000))
        guard let parsed = try? AttributedString(markdown: bounded) else { return bounded }
        var output = "", previous: Int?
        for run in parsed.runs {
            let block = run.presentationIntent?.components.first?.identity
            if previous != block, !output.isEmpty { output += " " }
            output += String(parsed[run.range].characters); previous = block
        }
        return output.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }
    static func sorted(_ agents: [ReadingAgent], query: String, needsReply: Bool = false, filter: AgentListFilter = .all) -> [ReadingAgent] {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return agents.filter {
            let matches: Bool
            switch filter {
            case .all: matches = true
            case .reply: matches = Self.needsReply($0)
            case .newResults: matches = ($0.unreadCount ?? 0) > 0
            case .active: matches = $0.currentRunID?.isEmpty == false
            }
            return matches && (!needsReply || Self.needsReply($0)) && (query.isEmpty ||
                $0.name.localizedCaseInsensitiveContains(query) || $0.id.localizedCaseInsensitiveContains(query))
        }.sorted {
            if Self.needsReply($0) != Self.needsReply($1) { return Self.needsReply($0) }
            let lhs = activityDate($0) ?? .distantPast, rhs = activityDate($1) ?? .distantPast
            if lhs != rhs { return lhs > rhs }
            return $0.name == $1.name ? $0.id < $1.id : $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }
}

enum ReadingStatus: String, Equatable {
    case disconnected, syncing, live, offline, permissionDenied, sessionExpired, incompatible
}

enum ReadingReadStatus: String, Equatable {
    case idle, pending, failed, confirmed
}

extension JSONValue {
    var readingString: String? {
        if case .string(let value) = self { return value }
        return nil
    }
    var readingInteger: Int64? {
        if case .integer(let value) = self { return value }
        return nil
    }
}

struct ReadingPartition: Hashable, Codable, Sendable {
    let api: String
    let network: String
    let runtime: String
    let user: String
    let visibility: String

    init?(apiBaseURL: URL, identity: HolonConnectionIdentity) {
        guard let runtime = identity.runtimeID, !runtime.isEmpty,
              let user = identity.userID, !user.isEmpty,
              let visibility = identity.visibilityScopeID, !visibility.isEmpty,
              var url = URLComponents(url: apiBaseURL, resolvingAgainstBaseURL: false),
              url.user == nil, url.password == nil else { return nil }
        url.query = nil
        url.fragment = nil
        guard let api = url.string else { return nil }
        self.api = api
        network = identity.networkID
        self.runtime = runtime
        self.user = user
        self.visibility = visibility
    }
}

/// Keep the server's live cursor authoritative while retaining a bounded history window.
enum ReadingSnapshotWindow {
    static let maximumTurns = 180
    static let maximumBytes = 262_144

    static func merging(_ live: HolonConversationSnapshot,
                        history: HolonConversationSnapshot?) throws -> HolonConversationSnapshot {
        guard let history, history.eventLogEpoch == live.eventLogEpoch else { return live }
        guard case .array(let liveTurns) = live.raw["turns"],
              case .array(let cachedTurns) = history.raw["turns"] else {
            throw HolonConversationError.malformedProtocol
        }
        let liveHistory = liveTurns.filter { $0["execution"]?["kind"] != .string("active") }
        let cachedHistory = cachedTurns.filter { $0["execution"]?["kind"] != .string("active") }
        let liveIDs = Set(liveHistory.compactMap { $0["turn_id"]?.readingString })
        let liveStart = liveHistory.compactMap { $0["key"]?["turn_index"]?.readingInteger }.min()
        let cachedStart = cachedHistory.compactMap { $0["key"]?["turn_index"]?.readingInteger }.min()
        let extendsHistory: Bool
        if let liveStart, let cachedStart {
            extendsHistory = cachedStart < liveStart
                && cachedHistory.contains { liveIDs.contains($0["turn_id"]?.readingString ?? "") }
        } else {
            extendsHistory = false
        }
        guard case .object(var fields) = history.raw else {
            throw HolonConversationError.malformedProtocol
        }
        // Cached executions cannot contribute activity to a confirmed live snapshot.
        fields["turns"] = .array(cachedHistory)
        fields["active_turns"] = .array([])
        if !extendsHistory {
            // Disjoint/contained cache is readable, but cannot advance the live history frontier.
            fields["has_more"] = .bool(live.hasMore)
            fields["next_before_cursor"] = live.nextBeforeCursor.map(JSONValue.string) ?? .null
        }
        let page = try HolonConversationSnapshot(raw: .object(fields))
        let merged = try mergeConversationHistory(live, page: page, maximumTurns: Int.max)
        guard case .array(let turns) = merged.raw["turns"],
              turns.filter({ $0["execution"]?["kind"] != .string("active") }).count <= maximumTurns else {
            // Capacity eviction falls back to a real server window, never an invented page cursor.
            return live
        }
        return merged
    }

    /// Run on a worker. An oversized history window falls back to the confirmed live tail.
    static func cacheCandidates(_ displayed: HolonConversationSnapshot,
                                liveWindow: HolonConversationSnapshot?) -> [JSONValue] {
        var values = [displayed.raw]
        if let liveWindow, liveWindow.eventLogEpoch == displayed.eventLogEpoch,
           liveWindow.raw != displayed.raw { values.append(liveWindow.raw) }
        return values.filter { ((try? JSONEncoder().encode($0).count) ?? Int.max) <= maximumBytes }
    }
}
