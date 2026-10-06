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
    func holdItems() { blockItems = true }
    func breakTasks() { failTasks = true }
    func blocked() -> Bool { gate != nil }
    func release() { blockItems = false; gate?.resume(); gate = nil }
    func items(agentID: String) async throws -> [WorkRecord] {
        calls += 1
        if blockItems { await withCheckedContinuation { gate = $0 } }
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
        try WorkRecord(raw: .object(["id": .string(id)]))
    }
    func task(agentID: String, id: String) async throws -> WorkRecord {
        try WorkRecord(raw: .object(["task_id": .string(id), "status": .string("future-state")]), task: true)
    }
    func output(agentID: String, id: String) async throws -> WorkOutput {
        try WorkOutput(raw: .object(["output_preview": .string("partial"),
                                     "output_truncated": .bool(true)]))
    }
    func brief(agentID: String, id: String) async throws -> JSONValue { .object(["body": .string(id)]) }
    func close() async { closed = true }
}

@MainActor
final class WorkCoordinatorTests: XCTestCase {
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
