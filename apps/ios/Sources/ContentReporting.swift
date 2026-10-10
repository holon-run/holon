import Foundation
import Observation
import HolonClient

struct ContentReportScope: Equatable, Identifiable {
    let sessionID: UUID
    let partition: ReadingPartition
    let agentID: String
    let epoch: String
    var id: UUID { sessionID }
}

struct ContentReportKey: Hashable {
    let turnID: String
    let messageID: String
}

enum ContentReportCategory: String, CaseIterable, Identifiable {
    case harmfulOrAbusive = "harmful_or_abusive"
    case sexualContent = "sexual_content"
    case hateOrHarassment = "hate_or_harassment"
    case selfHarm = "self_harm"
    case violence, privacy
    case spamOrOther = "spam_or_other"

    var id: String { rawValue }
    var title: String { "report.category." + rawValue }
}

struct ContentReportTarget: Equatable {
    let turnID: String
    let messageID: String
    let text: String

    init?(turnID: String, activity: ReadingActivity, detail: JSONValue) {
        guard activity.kind == "assistant", let id = activity.detailID,
              !turnID.isEmpty, turnID.unicodeScalars.count <= 256, id.unicodeScalars.count <= 256,
              detail["id"] == .string(id),
              detail["kind"] == .string("assistant_round") || detail["kind"] == .string("subagent_assistant_round") else { return nil }
        let data = ActivityPresentation.decoded(detail["data"] ?? .null)
        guard data["visibility"] == nil || data["visibility"] == .string("operator_visible"),
              data["turn_id"] == nil || data["turn_id"] == .string(turnID) else { return nil }
        let text: String
        if case .array(let blocks) = data["blocks"] {
            text = blocks.filter { $0["type"] == .string("text") }
                .compactMap { $0["text"]?.readingString }.joined(separator: "\n\n")
        } else if let value = data["text"]?.readingString {
            text = value
        } else if data["body"]?["type"] == .string("text") || data["body"]?["type"] == .string("brief") {
            text = data["body"]?["text"]?.readingString ?? ""
        } else { return nil }
        guard !text.isEmpty else { return nil }
        self.turnID = turnID; messageID = id; self.text = text
    }
}

/// An attempted report is immutable so retries cannot change an unknown submission.
@Observable @MainActor
final class ContentReportDraft {
    let scope: ContentReportScope
    let target: ContentReportTarget
    var category: ContentReportCategory?
    var explanation = ""
    private(set) var request: HolonContentReportRequest?
    private(set) var isSubmitting = false
    private(set) var receipt: HolonContentReportResponse?
    private(set) var errorKey: String?
    private let requestID = UUID().uuidString

    init(scope: ContentReportScope, target: ContentReportTarget) {
        self.scope = scope; self.target = target
    }

    var canSubmit: Bool {
        !isSubmitting && receipt == nil &&
        (request != nil || category != nil && explanation.unicodeScalars.count <= 2_000)
    }

    func submit(using operation: (HolonContentReportRequest, ContentReportScope) async throws -> HolonContentReportResponse) async {
        guard canSubmit else { return }
        if request == nil, let category {
            do {
                request = try HolonContentReportRequest(agentID: scope.agentID, turnID: target.turnID,
                    messageID: target.messageID, category: category.rawValue,
                    description: explanation.isEmpty ? nil : explanation, clientRequestID: requestID)
            } catch {
                errorKey = "report.invalidRequest"
                return
            }
        }
        guard let request else { return }
        isSubmitting = true; errorKey = nil
        defer { isSubmitting = false }
        do {
            let response = try await operation(request, scope)
            guard response.status == "accepted", !response.reportID.isEmpty else {
                throw HolonClientError.malformedResponse
            }
            receipt = response
        } catch let failure as HolonHTTPFailure {
            switch failure.statusCode {
            case 401: errorKey = "report.sessionExpired"
            case 403: errorKey = "report.permissionDenied"
            case 404: errorKey = "report.unavailable"
            case 409: errorKey = "report.conflict"
            case 429: errorKey = "report.rateLimited"
            default: errorKey = "report.unknownOutcome"
            }
        } catch is CancellationError {
            errorKey = "report.contextChanged"
        } catch { errorKey = "report.unknownOutcome" }
    }
}
