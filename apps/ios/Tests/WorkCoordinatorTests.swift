import Foundation
import HolonClient
import XCTest
@testable import Holon

private actor WorkFakeTransport: WorkTransport {
    private var gate: CheckedContinuation<Void, Never>?
    private var blockItems = false
    private var failTasks = false
    private(set) var calls = 0
    private(set) var closed = false
    private var itemCount: Int?
    private var resultBrief = false
    private var briefError = false
    private var running = false
    private(set) var outputCalls = 0
    private(set) var limits: [Int] = []
    func setItemCount(_ value: Int) { itemCount = value }
    func useResultBrief(failing: Bool) { resultBrief = true; briefError = failing }
    func useRunningTask() { running = true }
    func holdItems() { blockItems = true }
    func breakTasks() { failTasks = true }
    func blocked() -> Bool { gate != nil }
    func release() { blockItems = false; gate?.resume(); gate = nil }
    func items(agentID: String) async throws -> [WorkRecord] {
        try await items(agentID: agentID, limit: 50)
    }
    func items(agentID: String, limit: Int) async throws -> [WorkRecord] {
        calls += 1
        limits.append(limit)
        if blockItems { await withCheckedContinuation { gate = $0 } }
        if let itemCount { return try (0..<min(limit, itemCount)).map { try WorkRecord(raw: .object(["id": .string("work-\($0)")])) } }
        return [try WorkRecord(raw: .object(["id": .string("work-\(agentID)"),
                                           "objective": .string("Objective")]))]
    }
    func tasks(agentID: String) async throws -> [WorkRecord] {
        calls += 1
        if failTasks { throw WorkProtocolError.malformed }
        return [try WorkRecord(raw: .object(["task_id": .string("task-\(agentID)"),
                                           "status": .string("running")]), task: true)]
    }
    func item(agentID: String, id: String) async throws -> WorkRecord {
        try WorkRecord(raw: .object(["id": .string(id), "result_brief_id": resultBrief ? .string("result") : .null]))
    }
    func task(agentID: String, id: String) async throws -> WorkRecord {
        try WorkRecord(raw: .object(["task_id": .string(id), "status": .string(running ? (outputCalls >= 2 ? "completed" : "running") : "future-state")]), task: true)
    }
    func output(agentID: String, id: String) async throws -> WorkOutput {
        outputCalls += 1
        return try WorkOutput(raw: .object(["output_preview": .string("partial"),
                                     "output_truncated": .bool(true)]))
    }
    func brief(agentID: String, id: String) async throws -> JSONValue {
        if briefError { throw WorkProtocolError.malformed }; return .object(["body": .string(id)])
    }
    func close() async { closed = true }
}

@MainActor
final class WorkCoordinatorTests: XCTestCase {
    func testExplicitWorkWindowGrowsAndResetsOnAgentChange() async throws {
        let transport = WorkFakeTransport(); await transport.setItemCount(130)
        let coordinator = WorkCoordinator(); coordinator.selectAgent("A")
        coordinator.activate(transport: transport, identity: identity())
        try await wait { coordinator.itemsState == .loaded }; XCTAssertEqual(coordinator.items.count, 50)
        coordinator.loadMoreItems(); try await wait { coordinator.itemsState == .loaded }
        XCTAssertEqual(coordinator.items.count, 100)
        coordinator.loadMoreItems(); try await wait { coordinator.itemsState == .loaded }
        XCTAssertEqual(coordinator.items.count, 130); XCTAssertEqual(coordinator.itemLimit, 200)
        let limits = await transport.limits; XCTAssertEqual(limits, [50, 100, 200])
        coordinator.selectAgent("B"); XCTAssertEqual(coordinator.itemLimit, 50); coordinator.disconnect()
    }

    func testInlineBriefFailurePreservesWorkDetailsAndOffersSeparateRetryState() async throws {
        let transport = WorkFakeTransport(); await transport.useResultBrief(failing: true)
        let coordinator = WorkCoordinator(); coordinator.selectAgent("A")
        coordinator.activate(transport: transport, identity: identity()); coordinator.open(.item("work-A"))
        try await wait { coordinator.detailState == .loaded }
        XCTAssertEqual(coordinator.detail?.id, "work-A"); XCTAssertTrue(coordinator.briefFailed); XCTAssertNil(coordinator.brief)
        await transport.useResultBrief(failing: false); coordinator.open(.item("work-A"))
        try await wait { coordinator.detailState == .loaded }
        XCTAssertEqual(coordinator.brief?["body"], .string("result")); XCTAssertFalse(coordinator.briefFailed)
        coordinator.disconnect()
    }

    func testTaskOutputRefreshRunsOnlyForVisibleForegroundDetailAndStopsAtCompletion() async throws {
        let transport = WorkFakeTransport(); await transport.useRunningTask()
        let coordinator = WorkCoordinator(outputRefreshInterval: .milliseconds(10)); coordinator.selectAgent("A")
        coordinator.activate(transport: transport, identity: identity()); coordinator.open(.task("task-A"))
        try await wait { coordinator.detailState == .loaded }; try await Task.sleep(for: .milliseconds(30))
        let hidden = await transport.outputCalls; XCTAssertEqual(hidden, 1)
        coordinator.setDetailVisible(true)
        try await wait { coordinator.detail?.completed == true }
        let terminal = await transport.outputCalls
        try await Task.sleep(for: .milliseconds(40)); let after = await transport.outputCalls
        XCTAssertEqual(after, terminal)
        coordinator.setForeground(false); try await Task.sleep(for: .milliseconds(30))
        let background = await transport.outputCalls; XCTAssertEqual(background, terminal)
        coordinator.disconnect()
    }
    private func identity(user: String = "user") -> HolonConnectionIdentity {
        HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                userID: user, visibilityScopeID: "private")
    }
    private func wait(_ predicate: () async -> Bool) async throws {
        for _ in 0..<300 {
            if await predicate() { return }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("Timed out waiting for Work state")
    }

    func testItemsAndActiveTasksLoadIndependently() async throws {
        let transport = WorkFakeTransport()
        await transport.holdItems()
        let coordinator = WorkCoordinator()
        coordinator.selectAgent("A")
        coordinator.activate(transport: transport, identity: identity())
        try await wait { coordinator.tasksState == .loaded }
        XCTAssertEqual(coordinator.itemsState, .loading)
        XCTAssertEqual(coordinator.tasks.map(\.id), ["task-A"])
        await transport.release()
        try await wait { coordinator.itemsState == .loaded }
        XCTAssertEqual(coordinator.items.map(\.id), ["work-A"])
        coordinator.disconnect()
    }

    func testImmediateDisconnectDoesNotStartDeferredRequests() async throws {
        let transport = WorkFakeTransport()
        let coordinator = WorkCoordinator()
        coordinator.selectAgent("A")
        coordinator.activate(transport: transport, identity: identity())
        coordinator.disconnect()
        try await wait { await transport.closed }
        let calls = await transport.calls
        XCTAssertEqual(calls, 0)
        XCTAssertEqual(coordinator.itemsState, .disconnected)
    }

    func testLateResultCannotCrossUserAuthority() async throws {
        let old = WorkFakeTransport()
        await old.holdItems()
        let coordinator = WorkCoordinator()
        coordinator.selectAgent("A")
        coordinator.activate(transport: old, identity: identity())
        try await wait { await old.blocked() }
        coordinator.activate(transport: WorkFakeTransport(), identity: identity(user: "other"))
        try await wait { coordinator.itemsState == .loaded }
        coordinator.selectAgent("B")
        await old.release()
        try await wait { coordinator.items.first?.id == "work-B" }
        XCTAssertEqual(coordinator.tasks.first?.id, "task-B")
        coordinator.disconnect()
    }

    func testBackgroundRejectsAlreadyDeliveredResult() async throws {
        let transport = WorkFakeTransport()
        await transport.holdItems()
        let coordinator = WorkCoordinator()
        coordinator.selectAgent("A")
        coordinator.activate(transport: transport, identity: identity())
        try await wait { await transport.blocked() }
        coordinator.setForeground(false)
        await transport.release()
        for _ in 0..<5 { await Task.yield() }
        XCTAssertEqual(coordinator.itemsState, .offline)
        XCTAssertTrue(coordinator.items.isEmpty)
        coordinator.setForeground(true)
        try await wait { coordinator.itemsState == .loaded }
        coordinator.disconnect()
    }

    func testTaskFailureDoesNotDestroyWorkOrSessionAuthority() async throws {
        let transport = WorkFakeTransport()
        await transport.breakTasks()
        let coordinator = WorkCoordinator()
        var failures = 0
        coordinator.onConnectionFailure = { _ in failures += 1 }
        coordinator.selectAgent("A")
        coordinator.activate(transport: transport, identity: identity())
        try await wait { coordinator.itemsState == .loaded && coordinator.tasksState == .failed }
        XCTAssertEqual(coordinator.selectedAgentID, "A")
        XCTAssertEqual(coordinator.items.first?.id, "work-A")
        XCTAssertEqual(failures, 0)
        coordinator.disconnect()
    }

    func testTaskDetailPreservesUnknownStatusAndTruncatedOutput() async throws {
        let coordinator = WorkCoordinator()
        coordinator.selectAgent("A")
        coordinator.activate(transport: WorkFakeTransport(), identity: identity())
        coordinator.open(.task("task-A"))
        try await wait { coordinator.detailState == .loaded }
        XCTAssertEqual(coordinator.detail?.state, "future-state")
        XCTAssertEqual(coordinator.output?.status, "unknown")
        XCTAssertEqual(coordinator.output?.truncated, true)
        coordinator.open(.brief("brief"))
        try await wait { coordinator.detailState == .loaded }
        XCTAssertNil(coordinator.detail)
        XCTAssertNil(coordinator.output)
        XCTAssertEqual(coordinator.brief?["body"], .string("brief"))
        coordinator.disconnect()
    }
}
