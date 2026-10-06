import Foundation

/// An allowlist report, not a sanitizer over arbitrary logs or server error messages.
struct DiagnosticReport: Encodable, Equatable {
    let schema = "holon.ios.diagnostics.v1"
    let createdAt: Date
    let platform = "iOS"
    let connection: String
    let reading: String
    let sending: String
    let agentCount: Int
    let queuedCount: Int
    let unknownCount: Int
    let redacted = true

    init(createdAt: Date = Date(), connection: ConnectionStatus, reading: ReadingStatus,
         sending: SendingStatus, agentCount: Int, entries: [SendingEntry]) {
        self.createdAt = createdAt
        self.connection = connection.rawValue
        self.reading = reading.rawValue
        self.sending = sending.rawValue
        self.agentCount = max(0, agentCount)
        queuedCount = entries.filter { $0.state == .queued }.count
        unknownCount = entries.filter { $0.state == .unknown }.count
    }

    func text() throws -> String {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return String(decoding: try encoder.encode(self), as: UTF8.self)
    }
}
