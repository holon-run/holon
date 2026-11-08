import HolonClient
import XCTest
@testable import Holon

final class AgentIdentityPresentationTests: XCTestCase {
    func testStatusAndExpressionSharePrecedence() {
        let cases: [(String?, String?, String?, Int?, Int?, String, HolonAvatarExpression)] = [
            ("deleting", "active_turn", "run", 0, 0, "deleting", .neutral),
            ("stopped", "active_turn", "run", 0, 0, "stopped", .neutral),
            ("awake_running", "waiting_for_operator", nil, 0, 0, "running", .focused),
            (nil, "waiting_for_operator", "run", 0, 0, "running", .focused),
            ("asleep", "waiting_for_operator", nil, 0, 0, "needsInput", .questioning),
            ("asleep", "waiting_for_task", nil, 0, 1, "waiting", .waiting),
            ("awake_idle", "blocked", nil, 0, 0, "waiting", .waiting),
            ("asleep", "idle", nil, 0, 0, "sleeping", .sleeping),
            ("asleep", "idle", nil, 1, 0, "ready", .neutral),
            ("asleep", "idle", nil, 0, 1, "ready", .neutral),
            ("awake_idle", "idle", nil, 0, 0, "ready", .neutral),
            (nil, nil, nil, nil, nil, "unknown", .neutral),
        ]
        for (lifecycle, posture, run, pending, tasks, label, expression) in cases {
            let agent = ReadingAgent(id: "stable-id", name: "Name", preview: "", currentRunID: run,
                                     posture: posture, lifecycle: lifecycle, pendingCount: pending,
                                     activeTaskCount: tasks)
            XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: true),
                           .init(statusKey: "avatar.status." + label, expression: expression))
            XCTAssertEqual(AgentIdentityPresentation.derive(agent, current: false),
                           .init(statusKey: "avatar.status.offline", expression: .neutral))
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
