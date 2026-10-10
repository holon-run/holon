import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class ContentReportingTests: XCTestCase {
    private let identity = HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                                   userID: "user", visibilityScopeID: "private")

    private func activity(kind: String = "assistant") throws -> ReadingActivity {
        let id = kind + ":transcript_1"
        return try ReadingActivity(.object([
            "id": .string(id), "kind": .string(kind), "revision": .integer(1),
            "key": .object(["event_seq": .integer(1), "activity_id": .string(id)]),
            "summary": .string("preview only")
        ]))
    }

    private func detail(data: JSONValue? = nil, kind: String = "assistant_round", id: String = "transcript_1") -> JSONValue {
        .object(["id": .string(id), "agent_id": .string("A"), "kind": .string(kind),
                 "data": data ?? .object(["visibility": .string("operator_visible"), "text": .string("Full AI response")])])
    }

    private func draft() throws -> ContentReportDraft {
        let scope = ContentReportScope(sessionID: UUID(),
            partition: try XCTUnwrap(ReadingPartition(apiBaseURL: URL(string: "https://report.example/proxy/api")!,
                                                     identity: identity)), agentID: "A", epoch: "epoch")
        let target = try XCTUnwrap(ContentReportTarget(turnID: "turn", activity: activity(), detail: detail()))
        return ContentReportDraft(scope: scope, target: target)
    }

    private func receipt(status: String = "accepted", id: String = "report_1") throws -> HolonContentReportResponse {
        try JSONDecoder().decode(HolonContentReportResponse.self, from: JSONEncoder().encode(
            JSONValue.object(["report_id": .string(id), "status": .string(status), "created_at": .string("2026-10-10T00:00:00Z")])))
    }

    func testTargetUsesTranscriptEvidenceIDAndFullVisibleText() throws {
        let value = try XCTUnwrap(ContentReportTarget(turnID: "turn", activity: activity(), detail: detail()))
        XCTAssertEqual(value.messageID, "transcript_1")
        XCTAssertEqual(value.text, "Full AI response")
        XCTAssertNil(ContentReportTarget(turnID: "turn", activity: try activity(kind: "tool"), detail: detail()))
        XCTAssertNil(ContentReportTarget(turnID: "turn", activity: try activity(), detail: detail(kind: "incoming_message")))
        XCTAssertNil(ContentReportTarget(turnID: "turn", activity: try activity(), detail: detail(id: "other")))
    }

    func testFullResponseReaderKeepsContentBeyondPreviewWithoutActiveLinks() throws {
        let full = String(repeating: "report body ", count: 1_000) + "\nREPORT_TAIL_SENTINEL"
        let target = try XCTUnwrap(ContentReportTarget(turnID: "turn", activity: activity(),
            detail: detail(data: .object(["text": .string(full)]))))
        XCTAssertGreaterThan(target.text.count, 8_000)
        XCTAssertFalse(String(target.text.prefix(8_000)).contains("REPORT_TAIL_SENTINEL"))
        let native = RichTextSelectionView.makeTextView(text: target.text)
        XCTAssertEqual(native.text, full)
        XCTAssertTrue(native.text.hasSuffix("REPORT_TAIL_SENTINEL"))
        XCTAssertFalse(native.isEditable)
        XCTAssertTrue(native.isSelectable)
        XCTAssertTrue(native.dataDetectorTypes.isEmpty)
    }

    func testPrivateNonTextAndWrongTurnTargetsAreRejected() throws {
        for data: JSONValue in [
            .object(["visibility": .string("runtime_private"), "text": .string("private")]),
            .object(["turn_id": .string("other"), "text": .string("wrong turn")]),
            .object(["blocks": .array([.object(["type": .string("tool_use"), "text": .string("not text")])])])
        ] {
            XCTAssertNil(ContentReportTarget(turnID: "turn", activity: try activity(), detail: detail(data: data)))
        }
        let blocks: JSONValue = .object(["blocks": .array([
            .object(["type": .string("text"), "text": .string("one")]),
            .object(["type": .string("tool_use"), "text": .string("hidden")]),
            .object(["type": .string("text"), "text": .string("two")])
        ])])
        XCTAssertEqual(ContentReportTarget(turnID: "turn", activity: try activity(), detail: detail(data: blocks))?.text, "one\n\ntwo")
        XCTAssertEqual(ContentReportTarget(turnID: "turn", activity: try activity(),
            detail: detail(data: .object(["body": .object(["type": .string("brief"), "text": .string("brief text")])])))?.text, "brief text")
    }

    func testExplicitCategoryAndUnicodeScalarExplanationLimit() throws {
        let value = try draft()
        XCTAssertFalse(value.canSubmit)
        value.category = .privacy
        value.explanation = String(repeating: "e\u{301}", count: 1_000)
        XCTAssertTrue(value.canSubmit)
        value.explanation += "x"
        XCTAssertFalse(value.canSubmit)
        XCTAssertEqual(Set(ContentReportCategory.allCases.map(\.rawValue)), Set(HolonContentReportCategory.allCases.map(\.rawValue)))
    }

    func testUnknownOutcomeRetryRetainsExactPayloadAndNeverSendsConversation() async throws {
        let value = try draft()
        value.category = .privacy; value.explanation = "optional explanation"
        var attempted: [HolonContentReportRequest] = []
        await value.submit { body, _ in attempted.append(body); throw URLError(.timedOut) }
        XCTAssertNil(value.receipt)
        XCTAssertEqual(value.errorKey, "report.unknownOutcome")
        value.category = .violence; value.explanation = "changed after attempt"
        let accepted = try receipt()
        await value.submit { body, _ in attempted.append(body); return accepted }
        XCTAssertEqual(attempted.count, 2)
        XCTAssertEqual(attempted[0], attempted[1])
        XCTAssertEqual(attempted[1].category, "privacy")
        let raw = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(attempted[0]))
        XCTAssertNil(raw["text"]); XCTAssertNil(raw["messages"]); XCTAssertNil(raw["token"])
        XCTAssertEqual(value.receipt?.reportID, "report_1")
        XCTAssertFalse(value.canSubmit)
    }

    func testHTTPFailuresNeverCreateReceiptAndHaveSpecificMessages() async throws {
        for (status, key) in [(401, "sessionExpired"), (403, "permissionDenied"), (404, "unavailable"),
                              (409, "conflict"), (429, "rateLimited"), (500, "unknownOutcome")] {
            let value = try draft(); value.category = .spamOrOther
            await value.submit { _, _ in throw HolonHTTPFailure(statusCode: status, identity: self.identity) }
            XCTAssertNil(value.receipt)
            XCTAssertEqual(value.errorKey, "report." + key)
            XCTAssertNotNil(value.request?.clientRequestID)
        }
    }

    func testMalformedReceiptAndCancellationAreNotSuccess() async throws {
        for response in [try receipt(status: "resolved"), try receipt(id: "")] {
            let value = try draft(); value.category = .privacy
            await value.submit { _, _ in response }
            XCTAssertNil(value.receipt)
            XCTAssertEqual(value.errorKey, "report.unknownOutcome")
        }
        let value = try draft(); value.category = .privacy
        await value.submit { _, _ in throw CancellationError() }
        XCTAssertEqual(value.errorKey, "report.contextChanged")
        XCTAssertNil(value.receipt)
    }

    func testRepeatedTapCannotSubmitWhileOperationIsInFlight() async throws {
        let value = try draft(); value.category = .privacy
        let accepted = try receipt()
        var sends = 0
        await value.submit { _, _ in
            sends += 1
            XCTAssertFalse(value.canSubmit)
            await value.submit { _, _ in sends += 1; return accepted }
            return accepted
        }
        XCTAssertEqual(sends, 1)
        XCTAssertNotNil(value.receipt)
    }
}
