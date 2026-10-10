import Foundation

public enum HolonContentReportCategory: String, Codable, Sendable, CaseIterable {
    case harmfulOrAbusive = "harmful_or_abusive"
    case sexualContent = "sexual_content"
    case hateOrHarassment = "hate_or_harassment"
    case selfHarm = "self_harm"
    case violence
    case privacy
    case spamOrOther = "spam_or_other"
}

/// Retain the same request (including its clientRequestID) for explicit retries.
public struct HolonContentReportRequest: Codable, Sendable, Equatable {
    public let agentID: String
    public let turnID: String
    /// Transcript evidence_id: the assistant activity's detailID, not its full activity or brief ID.
    public let messageID: String
    public let category: String
    public let description: String?
    public let clientRequestID: String?

    public init(agentID: String, turnID: String, messageID: String,
                category: String, description: String? = nil,
                clientRequestID: String? = nil) throws {
        self.agentID = agentID
        self.turnID = turnID
        self.messageID = messageID
        self.category = category
        self.description = description
        self.clientRequestID = clientRequestID
        try validate()
    }

    enum CodingKeys: String, CodingKey {
        case agentID = "agent_id", turnID = "turn_id", messageID = "message_id"
        case category, description
        case clientRequestID = "client_request_id"
    }

    internal func validate() throws {
        guard [agentID, turnID, messageID].allSatisfy({
            !$0.isEmpty && $0.unicodeScalars.count <= 256
        }), HolonContentReportCategory(rawValue: category) != nil,
        description.map({ $0.unicodeScalars.count <= 2_000 }) ?? true,
        clientRequestID.map({ id in
        !id.isEmpty && id.utf8.count <= 128 &&
        id.utf8.allSatisfy({
            (65...90).contains($0) || (97...122).contains($0) ||
            (48...57).contains($0) || [45, 95, 46].contains($0)
        })
        }) ?? true else { throw HolonClientError.invalidRequest }
    }
}

public struct HolonContentReportResponse: Codable, Sendable, Equatable {
    public let reportID: String
    public let status: String
    public let createdAt: String

    enum CodingKeys: String, CodingKey {
        case reportID = "report_id", status, createdAt = "created_at"
    }
}
