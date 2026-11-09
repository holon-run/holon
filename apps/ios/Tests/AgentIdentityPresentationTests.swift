import HolonClient
import UIKit
import XCTest
@testable import Holon

final class AgentIdentityPresentationTests: XCTestCase {
    private struct Fixture: Decodable {
        struct Input: Decodable {
            let lifecycle: String?
            let posture: String?
            let currentRunId: String?
            let pending: Int?
            let activeTaskCount: Int?
            let waitingCount: Int?
            let stateIsCurrent: Bool?
        }
        struct Expected: Decodable { let kind: String; let icon: String; let tone: String }
        struct Case: Decodable { let name: String; let input: Input; let expected: Expected }
        let defaults: Input
        let cases: [Case]
    }

    func testNativeSymbolsAndReducedMotion() {
        let cases: [(AgentIdentityPresentation.Kind, String)] = [
            (.stopped, "xmark.circle"), (.needsInput, "exclamationmark.circle"),
            (.running, "arrow.triangle.2.circlepath"), (.runnable, "arrow.right.circle"),
            (.waiting, "clock"), (.blocked, "lock"), (.ready, "checkmark.circle"),
            (.unknown, "questionmark.circle"), (.deleting, "minus.circle"),
        ]
        for (kind, symbol) in cases {
            let value = AgentIdentityPresentation(kind: kind)
            XCTAssertEqual(value.icon.systemName, symbol)
            XCTAssertNotNil(UIImage(systemName: symbol))
            XCTAssertFalse(value.animates(reduceMotion: true))
            XCTAssertEqual(value.animates(reduceMotion: false), kind == .running)
        }
    }

    func testPreviewAndUnreadDoNotDetermineStatus() {
        var agent = ReadingAgent(id: "stable", name: "Name", preview: "Working", unreadCount: 10, posture: "idle")
        let expected = AgentIdentityPresentation.derive(agent, current: true)
        agent.preview = "Waiting for your input"
        agent.operatorPreview = "Run now"
        agent.unreadCount = 99
        XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: true), expected)
        XCTAssertEqual(expected.kind, .ready)
    }

    func testSharedStatusDisplayFixture() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<4 { root.deleteLastPathComponent() }
        let data = try Data(contentsOf: root.appendingPathComponent("tests/fixtures/agent-status-display.json"))
        let fixture = try JSONDecoder().decode(Fixture.self, from: data)
        XCTAssertFalse(fixture.cases.isEmpty)
        for item in fixture.cases {
            let input = item.input, defaults = fixture.defaults
            let agent = ReadingAgent(
                id: item.name, name: "Name", preview: "",
                currentRunID: input.currentRunId ?? defaults.currentRunId,
                posture: input.posture ?? defaults.posture,
                lifecycle: input.lifecycle ?? defaults.lifecycle,
                pendingCount: input.pending ?? defaults.pending,
                activeTaskCount: input.activeTaskCount ?? defaults.activeTaskCount,
                waitingCount: input.waitingCount ?? defaults.waitingCount
            )
            let value = AgentIdentityPresentation.derive(
                agent, current: input.stateIsCurrent ?? defaults.stateIsCurrent ?? false
            )
            XCTAssertEqual(value.kind.rawValue, item.expected.kind, item.name)
            XCTAssertEqual(value.icon.rawValue, item.expected.icon, item.name)
            XCTAssertEqual(value.tone.rawValue, item.expected.tone, item.name)
        }
    }

    func testDeletedLifecycleIsStopped() {
        let agent = ReadingAgent(id: "deleted", name: "Name", preview: "",
                                 currentRunID: "stale-run", posture: "active_turn", lifecycle: "deleted")
        XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: true).kind, .stopped)
    }

    func testCachedAndRefreshingIdentityIsUnknownWithSecondaryConnectionDetail() {
        let agent = ReadingAgent(id: "stable", name: "Name", preview: "", currentRunID: "run")
        let cases: [(ReadingStatus, Bool, Bool, Bool, String?)] = [
            (.live, false, false, false, nil),
            (.syncing, false, false, false, "reading.status.syncing"),
            (.syncing, true, true, false, "reading.cache.refreshing"),
            (.live, true, false, false, "reading.cache.saved"),
            (.live, false, true, false, "reading.status.syncing"),
            (.offline, true, false, false, "reading.cache.offline"),
            (.offline, false, false, false, "reading.status.offline"),
            (.offline, true, false, true, "reading.cache.refreshing"),
            (.permissionDenied, true, true, true, "reading.status.permissionDenied"),
            (.sessionExpired, true, true, false, "reading.status.sessionExpired"),
            (.incompatible, true, true, false, "reading.status.incompatible"),
        ]
        for (status, cached, refreshing, connecting, key) in cases {
            let value = AgentIdentityPresentation.derive(agent, status: status, isRosterCurrent: true,
                                                        isCached: cached,
                                                        isRefreshing: refreshing, connecting: connecting)
            XCTAssertEqual(value.connectionStatusKey, key)
            XCTAssertEqual(value.kind, key == nil ? .running : .unknown)
            XCTAssertEqual(value.expression, key == nil ? .focused : .neutral)
            XCTAssertEqual(value.animates(reduceMotion: false), key == nil)
        }
    }

    func testUnconfirmedRosterStaysNeutralEvenWhenConversationIsLive() {
        let agent = ReadingAgent(id: "stable", name: "Name", preview: "", currentRunID: "old-run")
        for refreshing in [false, true] {
            XCTAssertEqual(AgentIdentityPresentation.derive(
                agent, status: .live, isRosterCurrent: false, isCached: false, isRefreshing: refreshing
            ), .init(kind: .unknown, connectionStatusKey: refreshing ? "reading.cache.refreshing" : "reading.cache.saved"))
        }
    }

    func testStatusAndExpressionSharePrecedence() {
        let cases: [(String?, String?, String?, AgentIdentityPresentation.Kind, HolonAvatarExpression)] = [
            ("deleting", "active_turn", "run", .deleting, .neutral),
            ("stopped", "active_turn", "run", .stopped, .neutral),
            ("awake_running", "waiting_for_operator", nil, .needsInput, .questioning),
            (nil, "waiting_for_operator", "run", .needsInput, .questioning),
            ("asleep", "waiting_for_task", nil, .waiting, .waiting),
            ("awake_idle", "blocked", nil, .blocked, .neutral),
            ("asleep", "idle", nil, .ready, .neutral),
            (nil, "has_queued_input", nil, .runnable, .neutral),
            (nil, nil, nil, .unknown, .neutral),
        ]
        for (lifecycle, posture, run, kind, expression) in cases {
            let agent = ReadingAgent(id: "stable-id", name: "Name", preview: "", currentRunID: run,
                                     posture: posture, lifecycle: lifecycle,
                                     pendingCount: kind == .unknown ? 0 : 3, activeTaskCount: 2)
            let value = AgentIdentityPresentation.derive(agent, current: true)
            XCTAssertEqual(value.kind, kind)
            XCTAssertEqual(value.expression, expression)
            XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: false),
                           .init(kind: .unknown))
        }
    }

    func testNormalizesStatusButDoesNotInferSleepingFromMissingDataOrRename() {
        var agent = ReadingAgent(id: "same", name: "Original", preview: "",
                                 posture: "  WAITING FOR OPERATOR ", lifecycle: "Awake Idle")
        let expected = AgentIdentityPresentation.derive(agent, current: true)
        agent.name = "Completely different"
        XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: true), expected)
        XCTAssertEqual(expected.expression, .questioning)
        agent.lifecycle = "asleep"
        agent.posture = "idle"
        XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: true).expression, .neutral)
    }

    func testExistingReadingCacheDecodesWithoutNewMetadata() throws {
        let data = Data(#"{"id":"old","name":"Saved","preview":"Text","posture":"idle"}"#.utf8)
        let agent = try JSONDecoder().decode(ReadingAgent.self, from: data)
        XCTAssertNil(agent.lifecycle)
        XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: false).expression, .neutral)
    }
}
