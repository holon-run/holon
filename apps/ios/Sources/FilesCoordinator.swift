import Foundation
import Observation
import HolonClient

@Observable @MainActor
final class FilesCoordinator {
    private(set) var workspaces: [FilesWorkspace] = []
    private(set) var directory: FilesDirectory?
    private(set) var prepared: FilesPrepared?
    private(set) var selectedAgentID: String?
    private(set) var isLoading = false
    private(set) var failure: FilesFailure?
    var query = ""
    var showHidden = false
    var onConnectionFailure: ((HolonHTTPFailure) -> Void)?
    @ObservationIgnored private var transport: (any FilesTransport)?
    @ObservationIgnored private var identity: HolonConnectionIdentity?
    @ObservationIgnored private var foreground = true
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var operation = 0
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private let cache: FilesCache

    init() { cache = FilesCache() }
    init(cache: FilesCache) { self.cache = cache }

    var entries: [FilesEntry] {
        directory?.filtered(query: query, showHidden: showHidden) ?? []
    }

    func activate(client: HolonClient, identity: HolonConnectionIdentity) {
        activate(transport: FilesClientTransport(client: client, authority: identity), identity: identity)
    }

    func activate(transport: any FilesTransport, identity: HolonConnectionIdentity) {
        disconnect()
        self.transport = transport
        self.identity = identity
        do { try cache.bind(identity) } catch { failure = .forbidden }
    }

    func disconnect() {
        let previous = transport
        revision &+= 1
        cancelOperation()
        transport = nil
        identity = nil
        selectedAgentID = nil
        workspaces = []
        directory = nil
        failure = nil
        cache.clear()
        if let previous { Task { await previous.close() } }
    }

    func setForeground(_ value: Bool) {
        guard foreground != value else { return }
        foreground = value
        revision &+= 1
        cancelOperation()
        directory = nil
        workspaces = []
        if value, let id = selectedAgentID { selectAgent(id) }
    }

    func selectAgent(_ agentID: String?) {
        cancelOperation()
        selectedAgentID = agentID
        directory = nil
        workspaces = []
        query = ""
        guard let agentID else { return }
        run { transport in
            let result = try await transport.workspaces(agentID: agentID)
            return { self.workspaces = result }
        }
    }

    func browse(_ workspace: FilesWorkspace, path: String = "") {
        query = ""
        directory = nil
        run { transport in
            let result = try await transport.directory(workspace: workspace, path: path)
            guard result.workspace.workspaceID == workspace.workspaceID,
                  workspace.executionRootID == nil || result.workspace.executionRootID == workspace.executionRootID else {
                throw FilesFailure.invalidReference
            }
            return { self.directory = result }
        }
    }

    func openReference(_ reference: String) {
        prepare(.reference(reference))
    }

    @discardableResult
    func openPlan(agentID: String, workID: String, plan: JSONValue) -> Bool {
        guard accepts(agentID), !workID.isEmpty,
              string(plan["owner_agent_id"]) == agentID,
              let workspaceID = string(plan["workspace_id"]), !workspaceID.isEmpty,
              let path = string(plan["relative_path"]),
              path == "work-items/\(workID)/plan.md",
              !workID.contains("/"), !workID.contains("\\"),
              workID != ".", workID != ".." else { return rejectReference() }
        let requestedRoot = string(plan["execution_root_id"])
        if requestedRoot == "" { return rejectReference() }
        run { transport in
            let candidates = try await transport.workspaces(agentID: agentID).filter {
                $0.workspaceID == workspaceID &&
                    (requestedRoot == nil || $0.executionRootID == requestedRoot)
            }
            guard candidates.count == 1, var workspace = candidates.first else {
                throw FilesFailure.invalidReference
            }
            if workspace.executionRootID == nil {
                // Inactive attachments omit their root; pin the server's directory snapshot.
                let snapshot = try await transport.directory(workspace: workspace, path: "")
                guard snapshot.workspace.workspaceID == workspaceID, snapshot.path.isEmpty,
                      snapshot.workspace.executionRootID?.isEmpty == false else {
                    throw FilesFailure.invalidReference
                }
                workspace = snapshot.workspace
            }
            guard workspace.executionRootID?.isEmpty == false else {
                throw FilesFailure.invalidReference
            }
            let download = try await transport.download(
                source: .workspace(workspace, path: path), maximumBytes: FilesCache.maximumBytes)
            return { self.prepared = try self.cache.prepare(download) }
        }
        return true
    }

    @discardableResult
    func openArtifact(agentID: String, artifact: JSONValue) -> Bool {
        guard accepts(agentID),
              string(artifact["owner_agent_id"]).map({ $0 == agentID }) ?? true,
              let reference = string(artifact["ref"]), !reference.isEmpty,
              reference.utf8.count <= 8192,
              !reference.hasPrefix("/"), !reference.hasPrefix("file:") else {
            return rejectReference()
        }
        openReference(reference)
        return true
    }

    private func accepts(_ agentID: String) -> Bool {
        foreground && identity != nil && transport != nil &&
            !agentID.isEmpty && selectedAgentID == agentID
    }

    private func string(_ value: JSONValue?) -> String? {
        guard case .string(let text) = value else { return nil }
        return text
    }

    private func rejectReference() -> Bool {
        cancelOperation()
        failure = .invalidReference
        return false
    }

    func open(_ entry: FilesEntry) {
        guard let directory else { return }
        if entry.isDirectory { browse(directory.workspace, path: entry.path) }
        else { prepare(.workspace(directory.workspace, path: entry.path)) }
    }

    func dismissPreview() { cancelOperation() }

    private func prepare(_ source: FilesSource) {
        run { transport in
            let download = try await transport.download(source: source, maximumBytes: FilesCache.maximumBytes)
            return { self.prepared = try self.cache.prepare(download) }
        }
    }

    /// Revalidate the SDK generation immediately before handing a local file to a system surface.
    func authorizeExport(_ artifactID: UUID) async -> URL? {
        guard foreground, let identity, let transport, let artifact = prepared,
              artifact.id == artifactID, cache.owns(artifact) else { return nil }
        let capturedRevision = revision
        let capturedOperation = operation
        do {
            try await transport.validate()
            guard current(identity, capturedRevision, capturedOperation),
                  prepared?.id == artifactID, cache.owns(artifact) else { return nil }
            return artifact.url
        } catch {
            guard current(identity, capturedRevision, capturedOperation) else { return nil }
            report(error)
            prepared = nil
            cache.removeContents()
            return nil
        }
    }

    private func cancelOperation() {
        operation &+= 1
        task?.cancel()
        task = nil
        isLoading = false
        prepared = nil
        cache.removeContents()
        failure = nil
    }

    private func current(_ identity: HolonConnectionIdentity, _ revision: Int, _ operation: Int) -> Bool {
        foreground && self.identity == identity && self.revision == revision && self.operation == operation
    }

    private func run(
        _ body: @escaping @MainActor (any FilesTransport) async throws -> (@MainActor () throws -> Void)
    ) {
        cancelOperation()
        guard foreground, let transport, let identity else { return }
        let capturedRevision = revision
        let capturedOperation = operation
        isLoading = true
        task = Task { [weak self] in
            guard let self, self.current(identity, capturedRevision, capturedOperation), !Task.isCancelled else { return }
            do {
                try await transport.validate()
                let apply = try await body(transport)
                try await transport.validate()
                guard self.current(identity, capturedRevision, capturedOperation), !Task.isCancelled else { return }
                try apply()
            } catch {
                guard self.current(identity, capturedRevision, capturedOperation), !Task.isCancelled else { return }
                self.report(error)
                self.cache.removeContents()
                self.prepared = nil
            }
            guard self.current(identity, capturedRevision, capturedOperation) else { return }
            self.isLoading = false
            self.task = nil
        }
    }

    private func report(_ error: any Error) {
        if let http = error as? HolonHTTPFailure {
            switch http.statusCode {
            case 401, 403:
                failure = .forbidden
                onConnectionFailure?(http)
            case 404, 410: failure = .deleted
            case 413: failure = .tooLarge
            case 415: failure = .unsupported
            default: failure = .unavailable
            }
        } else if error is CancellationError {
            failure = .forbidden
        } else {
            failure = error as? FilesFailure ?? .unavailable
        }
    }
}
