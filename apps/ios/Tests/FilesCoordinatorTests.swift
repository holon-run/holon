import Foundation
import HolonClient
import XCTest
@testable import Holon

private actor FilesFakeTransport: FilesTransport {
    private var gate: CheckedContinuation<Void, Never>?
    private var waits = false
    private var entered: CheckedContinuation<Void, Never>?
    private var valid = true
    private(set) var calls = 0
    private(set) var roots: [String?] = []
    private(set) var sources: [FilesSource] = []
    private var pendingFailure: FilesFailure?
    private var workspaceList: [FilesWorkspace]?
    private var directoryResult: FilesDirectory?
    private var location: HolonFileLocation?
    private var directoryFailure: FilesFailure?
    func failDirectory(_ value: FilesFailure) { directoryFailure = value }
    func setLocation(_ value: HolonFileLocation) { location = value }

    func setWorkspaces(_ value: [FilesWorkspace]) { workspaceList = value }
    func setDirectory(_ value: FilesDirectory) { directoryResult = value }
    func block() { waits = true }
    func waitForEntry() async {
        if gate != nil { return }
        await withCheckedContinuation { entered = $0 }
    }
    func release() {
        waits = false
        gate?.resume()
        gate = nil
    }
    func invalidate() { valid = false }
    func fail(_ error: FilesFailure) { pendingFailure = error }
    func validate() throws {
        if !valid { throw CancellationError() }
    }
    func workspaces(agentID: String) async throws -> [FilesWorkspace] {
        calls += 1
        return workspaceList ?? [
            FilesWorkspace(workspaceID: "ws", executionRootID: "retained-worktree", name: agentID)
        ]
    }
    func directory(workspace: FilesWorkspace, path: String) async throws -> FilesDirectory {
        calls += 1
        roots.append(workspace.executionRootID)
        if let directoryFailure { throw directoryFailure }
        if waits {
            await withCheckedContinuation { continuation in
                gate = continuation
                entered?.resume()
                entered = nil
            }
        }
        return directoryResult ?? FilesDirectory(workspace: workspace, path: path, entries: [])
    }
    func download(source: FilesSource, maximumBytes: Int) async throws -> FilesDownload {
        calls += 1
        sources.append(source)
        if waits {
            await withCheckedContinuation { continuation in
                gate = continuation
                entered?.resume()
                entered = nil
            }
        }
        if let pendingFailure { throw pendingFailure }
        return FilesDownload(data: Data("private".utf8), mediaType: "text/plain", name: "private.txt", location: location)
    }
    func close() {}
}

@MainActor
final class FilesCoordinatorTests: XCTestCase {
    func testForegroundResumesOriginalResolvedRootAndReadingMetadata() async throws {
        let transport = FilesFakeTransport(), coordinator = FilesCoordinator()
        let location = try HolonFileLocation(raw: .object([
            "workspace_id": .string("ws"), "execution_root_id": .string("original-root"),
            "path": .string("private.txt"), "absolute_path": .string("/original/private.txt"),
            "root_kind": .string("git_worktree_root"), "kind": .string("file")
        ]))
        await transport.setLocation(location)
        coordinator.activate(transport: transport, identity: identity()); coordinator.selectAgent("A")
        await settle(coordinator)
        let request = FilesRequest.source(.reference("/original/private.txt"))
        coordinator.openRequest(request); await settle(coordinator)
        let oldURL = try XCTUnwrap(coordinator.prepared?.url)
        let position = FilesReadingPosition(page: 8, atEnd: true, renderMarkdown: false, wrap: false)
        coordinator.rememberPosition(position, for: request)
        coordinator.setForeground(false)
        XCTAssertNil(coordinator.prepared); XCTAssertFalse(FileManager.default.fileExists(atPath: oldURL.path))
        coordinator.setForeground(true); await settle(coordinator)
        let sources = await transport.sources
        XCTAssertEqual(sources.last, .workspace(.init(workspaceID: "ws", executionRootID: "original-root", name: "ws"), path: "private.txt"))
        XCTAssertEqual(coordinator.readingPosition(for: request), position)
        XCTAssertNotNil(coordinator.prepared)
        coordinator.selectAgent("B"); XCTAssertEqual(coordinator.readingPosition(for: request), .init())
        coordinator.disconnect()
    }

    func testForegroundRestoresFilteredDirectoryAndRejectsRemovedRoot() async {
        let transport = FilesFakeTransport(), coordinator = FilesCoordinator()
        let workspace = FilesWorkspace(workspaceID: "ws", executionRootID: "r", name: "Source")
        coordinator.activate(transport: transport, identity: identity()); coordinator.selectAgent("A")
        await settle(coordinator); coordinator.browse(workspace, path: "docs"); await settle(coordinator)
        coordinator.query = "report"; coordinator.directoryPosition = "docs/report.md"; coordinator.sort = .modified
        coordinator.setForeground(false); coordinator.setForeground(true); await settle(coordinator)
        XCTAssertEqual(coordinator.directory?.path, "docs"); XCTAssertEqual(coordinator.query, "report")
        XCTAssertEqual(coordinator.directoryPosition, "docs/report.md"); XCTAssertEqual(coordinator.sort, .modified)
        await transport.setDirectory(.init(workspace: .init(workspaceID: "ws", executionRootID: "different", name: "Source"), path: "docs", entries: []))
        coordinator.setForeground(false); coordinator.setForeground(true); await settle(coordinator)
        XCTAssertEqual(coordinator.browserFailure, .invalidReference); XCTAssertNil(coordinator.directory)
        coordinator.disconnect()
    }

    func testRemovedOldBrowserRootCannotBlockAValidCurrentFileInAnotherRoot() async throws {
        let transport = FilesFakeTransport(), coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity()); coordinator.selectAgent("A")
        await settle(coordinator)
        coordinator.browse(.init(workspaceID: "project", executionRootID: "removed-worktree", name: "Project"))
        await settle(coordinator)
        let location = try HolonFileLocation(raw: .object([
            "workspace_id": .string("home"), "execution_root_id": .string("live-home"), "path": .string("plan.md"),
            "absolute_path": .string("/home/plan.md"), "kind": .string("file"), "root_kind": .string("canonical_root")
        ]))
        await transport.setLocation(location)
        let request = FilesRequest.source(.reference("/home/plan.md"))
        coordinator.openRequest(request); await settle(coordinator)
        coordinator.rememberPosition(.init(page: 3), for: request)
        await transport.failDirectory(.rootUnavailable)
        coordinator.setForeground(false); coordinator.setForeground(true); await settle(coordinator)
        XCTAssertNotNil(coordinator.prepared); XCTAssertNil(coordinator.failure)
        XCTAssertEqual(coordinator.browserFailure, .rootUnavailable)
        XCTAssertEqual(coordinator.readingPosition(for: request).page, 3)
        let sources = await transport.sources
        XCTAssertEqual(sources.last, .workspace(.init(workspaceID: "home", executionRootID: "live-home", name: "home"), path: "plan.md"))
        coordinator.disconnect()
    }

    func testCancelledDownloadRetainsRetryRequestButNotStaleBytes() async {
        let transport = FilesFakeTransport(), coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        await transport.block(); coordinator.openReference("/private.txt"); await transport.waitForEntry()
        coordinator.cancelDownload(); await transport.release()
        for _ in 0..<10 { await Task.yield() }
        XCTAssertTrue(coordinator.cancelled); XCTAssertNil(coordinator.prepared)
        XCTAssertEqual(coordinator.request, .source(.reference("/private.txt")))
        coordinator.openReference("/private.txt"); await settle(coordinator)
        XCTAssertNotNil(coordinator.prepared); XCTAssertFalse(coordinator.cancelled)
        coordinator.disconnect()
    }
    func testReturningFromPlanRestoresRootLoadCancelledByPlan() async {
        let transport = FilesFakeTransport()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.selectAgent("A")
        XCTAssertTrue(coordinator.openPlan(agentID: "A", workID: "work", plan: .object([
            "owner_agent_id": .string("A"), "workspace_id": .string("ws"),
            "relative_path": .string("work-items/work/plan.md")
        ])))
        await settle(coordinator)
        XCTAssertNotNil(coordinator.prepared)
        XCTAssertTrue(coordinator.workspaces.isEmpty)
        coordinator.dismissPreview()
        await settle(coordinator)
        XCTAssertNil(coordinator.prepared)
        XCTAssertEqual(coordinator.workspaces.map(\.workspaceID), ["ws"])
    }

    func testPlanUsesServerRootAndRejectsOwnerAndAbsolutePath() async {
        let transport = FilesFakeTransport()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.selectAgent("A")
        await settle(coordinator)
        let plan: JSONValue = .object([
            "owner_agent_id": .string("A"), "workspace_id": .string("ws"),
            "relative_path": .string("work-items/work/plan.md"),
            "path": .string("/machine/path/ignored")
        ])
        XCTAssertTrue(coordinator.openPlan(agentID: "A", workID: "work", plan: plan))
        await settle(coordinator)
        let sources = await transport.sources
        XCTAssertEqual(sources, [.workspace(
            FilesWorkspace(workspaceID: "ws", executionRootID: "retained-worktree", name: "A"),
            path: "work-items/work/plan.md")])
        XCTAssertFalse(coordinator.openPlan(agentID: "B", workID: "work", plan: plan))
        XCTAssertFalse(coordinator.openPlan(agentID: "A", workID: "../work", plan: plan))
        XCTAssertFalse(coordinator.openArtifact(agentID: "A", artifact: .object([
            "ref": .string("/machine/path")
        ])))
    }

    func testPlanRejectsUnavailableRootWithoutDownloading() async {
        let transport = FilesFakeTransport()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.selectAgent("A")
        await settle(coordinator)
        XCTAssertTrue(coordinator.openPlan(agentID: "A", workID: "work", plan: .object([
            "owner_agent_id": .string("A"), "workspace_id": .string("ws"),
            "relative_path": .string("work-items/work/plan.md"),
            "execution_root_id": .string("removed-root")
        ])))
        await settle(coordinator)
        XCTAssertEqual(coordinator.failure, .invalidReference)
        let sources = await transport.sources
        XCTAssertTrue(sources.isEmpty)
    }

    func testPlanPinsInactiveHomeDirectoryRootRatherThanActiveProject() async {
        let transport = FilesFakeTransport()
        let home = FilesWorkspace(workspaceID: "agent_home:A", executionRootID: nil, name: "Home")
        let resolved = FilesWorkspace(workspaceID: home.workspaceID,
                                      executionRootID: "server-home-root", name: home.name)
        await transport.setWorkspaces([
            FilesWorkspace(workspaceID: "project", executionRootID: "active-worktree", name: "Project"),
            home
        ])
        await transport.setDirectory(FilesDirectory(workspace: resolved, path: "", entries: []))
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.selectAgent("A")
        await settle(coordinator)
        XCTAssertTrue(coordinator.openPlan(agentID: "A", workID: "work", plan: .object([
            "owner_agent_id": .string("A"), "workspace_id": .string(home.workspaceID),
            "relative_path": .string("work-items/work/plan.md"),
            "path": .string("/server/agent/home/work-items/work/plan.md")
        ])))
        await settle(coordinator)
        XCTAssertNil(coordinator.failure)
        XCTAssertNotNil(coordinator.prepared)
        let roots = await transport.roots
        let sources = await transport.sources
        XCTAssertEqual(roots, [nil])
        XCTAssertEqual(sources, [.workspace(resolved, path: "work-items/work/plan.md")])
    }

    func testPlanRejectsMismatchedOrMissingDirectoryAuthorityWithoutDownload() async {
        let home = FilesWorkspace(workspaceID: "agent_home:A", executionRootID: nil, name: "Home")
        let snapshots = [
            FilesDirectory(workspace: FilesWorkspace(workspaceID: "project", executionRootID: "root",
                                                    name: "Project"), path: "", entries: []),
            FilesDirectory(workspace: FilesWorkspace(workspaceID: home.workspaceID, executionRootID: "root",
                                                    name: home.name), path: "different", entries: []),
            FilesDirectory(workspace: home, path: "", entries: []),
            FilesDirectory(workspace: FilesWorkspace(workspaceID: home.workspaceID, executionRootID: "",
                                                    name: home.name), path: "", entries: [])
        ]
        for snapshot in snapshots {
            let transport = FilesFakeTransport()
            await transport.setWorkspaces([home])
            await transport.setDirectory(snapshot)
            let coordinator = FilesCoordinator()
            coordinator.activate(transport: transport, identity: identity())
            coordinator.selectAgent("A")
            await settle(coordinator)
            XCTAssertTrue(coordinator.openPlan(agentID: "A", workID: "work", plan: .object([
                "owner_agent_id": .string("A"), "workspace_id": .string(home.workspaceID),
                "relative_path": .string("work-items/work/plan.md")
            ])))
            await settle(coordinator)
            XCTAssertEqual(coordinator.failure, .invalidReference)
            XCTAssertNil(coordinator.prepared)
            let sources = await transport.sources
            XCTAssertTrue(sources.isEmpty)
        }
    }

    private func identity(_ user: String = "user") -> HolonConnectionIdentity {
        HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                userID: user, visibilityScopeID: "private")
    }

    private func settle(_ coordinator: FilesCoordinator) async {
        for _ in 0..<200 {
            if !coordinator.isLoading { return }
            await Task.yield()
        }
        XCTFail("Files operation did not settle")
    }

    func testImmediateDisconnectBeforeMainActorTaskStartsMakesNoRequest() async {
        let transport = FilesFakeTransport()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.selectAgent("A")
        coordinator.disconnect()
        await Task.yield()
        let count = await transport.calls
        XCTAssertEqual(count, 0)
        XCTAssertTrue(coordinator.workspaces.isEmpty)
    }

    func testLateDirectoryCannotReplaceNewIdentity() async {
        let first = FilesFakeTransport()
        await first.block()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: first, identity: identity())
        coordinator.browse(FilesWorkspace(workspaceID: "ws", executionRootID: "old-root", name: "old"))
        await first.waitForEntry()
        coordinator.activate(transport: FilesFakeTransport(), identity: identity("other"))
        await first.release()
        for _ in 0..<10 { await Task.yield() }
        XCTAssertNil(coordinator.directory)
        XCTAssertNil(coordinator.prepared)
    }

    func testOperationGenerationRejectsLateDownloadAndBackgroundCancels() async {
        let transport = FilesFakeTransport()
        await transport.block()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.openReference("/source/private.txt")
        await transport.waitForEntry()
        coordinator.setForeground(false)
        await transport.release()
        for _ in 0..<10 { await Task.yield() }
        XCTAssertNil(coordinator.prepared)
        XCTAssertFalse(coordinator.isLoading)
        coordinator.openReference("/source/private.txt")
        let count = await transport.calls
        XCTAssertEqual(count, 1)
    }

    func testRootPreservedAndExportRequiresCurrentSDKIdentity() async {
        let transport = FilesFakeTransport()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.browse(FilesWorkspace(workspaceID: "ws", executionRootID: "source-worktree", name: "source"))
        await settle(coordinator)
        let roots = await transport.roots
        XCTAssertEqual(roots, ["source-worktree"])
        coordinator.openReference("workspace://ws/private.txt?root=source-worktree")
        await settle(coordinator)
        guard let artifact = coordinator.prepared else { return XCTFail("Missing preview") }
        let authorized = await coordinator.authorizeExport(artifact.id)
        XCTAssertEqual(authorized, artifact.url)
        await transport.invalidate()
        let stale = await coordinator.authorizeExport(artifact.id)
        XCTAssertNil(stale)
        XCTAssertNil(coordinator.prepared)
        XCTAssertFalse(FileManager.default.fileExists(atPath: artifact.url.path))
        coordinator.disconnect()
    }

    func testReferenceFailureIsExplicitAndOldContentIsRemoved() async {
        let transport = FilesFakeTransport()
        let coordinator = FilesCoordinator()
        coordinator.activate(transport: transport, identity: identity())
        coordinator.openReference("/source/private.txt")
        await settle(coordinator)
        let old = coordinator.prepared?.url
        await transport.fail(.deleted)
        coordinator.openReference("/source/deleted.txt")
        await settle(coordinator)
        XCTAssertEqual(coordinator.failure, .deleted)
        XCTAssertNil(coordinator.prepared)
        XCTAssertFalse(old.map { FileManager.default.fileExists(atPath: $0.path) } ?? true)
        coordinator.disconnect()
    }
}
