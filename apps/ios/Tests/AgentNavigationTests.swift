import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class AgentNavigationTests: XCTestCase {
    private func partition(_ user: String = "user") -> ReadingPartition {
        ReadingPartition(apiBaseURL: URL(string: "https://host.example/api")!, identity: HolonConnectionIdentity(
            networkID: "network", runtimeID: "runtime", userID: user, visibilityScopeID: "private"))!
    }
    func testChildRoutesKeepSelectionAndSinglePopReturnsHome() {
        let router = AppRouter(preferences: nil)
        router.activate(partition())
        router.path = [.conversation("A"), .work("A"), .workDetail("A", .item("work"))]
        XCTAssertEqual(router.agentID, "A")
        router.path.removeLast(2)
        XCTAssertEqual(router.path, [.conversation("A")])
        router.path.removeLast()
        XCTAssertNil(router.agentID)
        XCTAssertTrue(router.path.isEmpty)
    }
    func testPermissionWithdrawalRemovesAgentDestinationsAndStaleBookmark() {
        let router = AppRouter(preferences: nil)
        router.activate(partition())
        router.path = [.conversation("A"), .work("A"), .workDetail("A", .item("work"))]
        router.withdrawAgent()
        XCTAssertTrue(router.path.isEmpty)
        XCTAssertNil(router.agentID)
    }
    func testRestorationRequiresExactAuthorityAndRosterMembership() {
        let suite = "HolonNavigationTests." + UUID().uuidString
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let first = AppRouter(preferences: preferences)
        first.activate(partition())
        first.path = [.conversation("A")]
        first.remember()
        let restored = AppRouter(preferences: preferences)
        restored.activate(partition())
        restored.restore(agents: [ReadingAgent(id: "A", name: "A", preview: "")], authoritative: false)
        XCTAssertTrue(restored.path.isEmpty)
        restored.restore(agents: [ReadingAgent(id: "A", name: "A", preview: "")], authoritative: true)
        XCTAssertEqual(restored.path, [.conversation("A")])
        restored.activate(partition("other"))
        restored.restore(agents: [ReadingAgent(id: "A", name: "A", preview: "")], authoritative: true)
        XCTAssertTrue(restored.path.isEmpty)
        restored.activate(partition())
        restored.restore(agents: [], authoritative: true)
        XCTAssertTrue(restored.path.isEmpty)
        restored.activate(nil)
        XCTAssertNil(preferences.data(forKey: "navigation.confirmedAgent.v1"))
    }
    func testPreviewSortAndSearchBeyondFirstWindow() {
        let time = Date(timeIntervalSince1970: 100)
        let old = ReadingAgent(id: "old", name: "Old", preview: "result", operatorPreview: "old input",
                               briefAt: time, operatorAt: time.addingTimeInterval(-1))
        let input = ReadingAgent(id: "new", name: "New", preview: "old result", operatorPreview: "new input",
                                 briefAt: time, operatorAt: time.addingTimeInterval(1))
        let reply = ReadingAgent(id: "reply", name: "Reply", preview: "question", posture: "waiting_for_operator")
        XCTAssertEqual(AgentSummaryPresentation.preview(old), "result")
        XCTAssertEqual(AgentSummaryPresentation.preview(input), "new input")
        XCTAssertEqual(AgentSummaryPresentation.sorted([old, input, reply], query: "", needsReply: false).map(\.id),
                       ["reply", "new", "old"])
        let many = (0..<120).map { ReadingAgent(id: "A\($0)", name: "Agent \($0)", preview: "") }
        XCTAssertEqual(AgentSummaryPresentation.sorted(many, query: "A119", needsReply: false).map(\.id), ["A119"])
    }
    func testHomeFiltersUseKnownAuthorityAndBriefPreviewDoesNotChangeOperatorText() {
        let ready = ReadingAgent(id: "ready", name: "Ready", preview: "## Result\n\n**Done**\n\n- first", unreadCount: 2)
        let active = ReadingAgent(id: "active", name: "Active", preview: "", currentRunID: "r")
        let unknown = ReadingAgent(id: "unknown", name: "Unknown", preview: "", unreadCount: nil, posture: "future")
        XCTAssertEqual(AgentSummaryPresentation.preview(ready), "Result Done first")
        XCTAssertEqual(AgentSummaryPresentation.sorted([ready, active, unknown], query: "", filter: .newResults).map(\.id), ["ready"])
        XCTAssertEqual(AgentSummaryPresentation.sorted([ready, active, unknown], query: "", filter: .active).map(\.id), ["active"])
        let input = ReadingAgent(id: "input", name: "Input", preview: "", operatorPreview: "# literal\n**input**")
        XCTAssertEqual(AgentSummaryPresentation.preview(input), "# literal\n**input**")
    }
    func testCanonicalInputReplacesOnlyTheJoinedLocalRequest() {
        let scope = SendingScope(partition: partition(), agentID: "A")
        var received = SendingEntry(requestID: UUID(), scope: scope, draft: SendingDraft(text: "hello"))
        received.state = .received
        received.messageID = "m"
        let unknown = SendingEntry(requestID: UUID(), scope: scope, draft: SendingDraft(text: "unknown"), state: .unknown)
        let raw: JSONValue = .object(["pending_inputs": .array([.object(["message_id": .string("m")])])])
        XCTAssertEqual(LocalMessageProjection.visible([received, unknown], canonicalIDs: LocalMessageProjection.canonicalIDs(raw)), [unknown])
        XCTAssertEqual(LocalMessageProjection.visible([received, unknown], canonicalIDs: []), [received, unknown])
        received.canonicalObserved = true
        XCTAssertEqual(LocalMessageProjection.visible([received, unknown], canonicalIDs: []), [unknown])
    }
}
