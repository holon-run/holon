import Foundation

struct SendingScope: Hashable, Codable, Sendable {
    let partition: ReadingPartition
    let agentID: String
}

struct SendingAttachment: Identifiable, Equatable, Codable, Sendable {
    let id: UUID
    let name: String
    let byteCount: Int64
    let contentType: String
}

struct SendingDraft: Equatable, Codable, Sendable {
    var text = ""
    var modelID: String?
    var attachments: [SendingAttachment] = []
}

struct SendingPayload: Equatable, Codable, Sendable {
    let text: String
    let modelID: String?
    let attachments: [SendingPreparedAttachment]
}

/// Current server accepts inline attachments, not a separate upload endpoint.
struct SendingPreparedAttachment: Equatable, Codable, Sendable {
    let name: String
    let contentType: String
    let data: Data
}

enum SendingState: String, Codable, Sendable {
    case queued, sending, received, failed, unknown
}

struct SendingEntry: Identifiable, Equatable, Codable, Sendable {
    var id: UUID { requestID }
    let requestID: UUID
    let scope: SendingScope
    let draft: SendingDraft
    var uploaded: [UUID: SendingPreparedAttachment] = [:]
    var payload: SendingPayload?
    var state: SendingState = .queued
    var messageID: String?
    var canonicalObserved: Bool?
    var error: String?
}

struct SendingModel: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
}

enum SendingStatus: String { case disconnected, ready, offline, sending }

enum SendingFailure: Error, LocalizedError, Sendable {
    case missingAttachment(String), oversizedAttachment(String), emptyDraft, unavailable
    case rejected(String)
    var errorDescription: String? {
        switch self {
        case .missingAttachment(let name): return "Attachment missing: \(name)"
        case .oversizedAttachment(let name): return "Attachment exceeds size limit: \(name)"
        case .emptyDraft: return "The draft is empty."
        case .unavailable: return "Offline: reconnect before sending."
        case .rejected(let message): return message
        }
    }
}
