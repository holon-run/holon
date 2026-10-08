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

enum AgentListFilter: String, CaseIterable { case all, reply, newResults, active }

enum AgentSummaryPresentation {
    static func needsReply(_ agent: ReadingAgent) -> Bool { agent.posture == "waiting_for_operator" }
    static func activityDate(_ agent: ReadingAgent) -> Date? {
        [agent.briefAt, agent.operatorAt].compactMap { $0 }.max()
    }
    static func preview(_ agent: ReadingAgent) -> String {
        if let input = agent.operatorPreview,
           agent.briefAt == nil || (agent.operatorAt.map { $0 > agent.briefAt! } == true) { return input }
        return plainBriefPreview(agent.preview)
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
