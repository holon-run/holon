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
    private(set) var request: FilesRequest?
    private(set) var preparedRequest: FilesRequest?
    private(set) var progress: HolonDownloadProgress?
    private(set) var cancelled = false
    var query = ""
    var showHidden = false
    var sort: FilesSort = .name
    var directoryPosition: String?
    var onConnectionFailure: ((HolonHTTPFailure) -> Void)?
    @ObservationIgnored private var transport: (any FilesTransport)?
    @ObservationIgnored private var identity: HolonConnectionIdentity?
    @ObservationIgnored private var foreground = true
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var operation = 0
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private let cache: FilesCache
    @ObservationIgnored private var pins: [FilesRequest: FilesSource] = [:]
    @ObservationIgnored private var positions: [FilesRequest: FilesReadingPosition] = [:]
    @ObservationIgnored private var folder: (FilesWorkspace, String)?

    init() { cache = FilesCache() }
    init(cache: FilesCache) { self.cache = cache }

    var entries: [FilesEntry] {
        directory?.filtered(query: query, showHidden: showHidden, sort: sort) ?? []
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
        request = nil; preparedRequest = nil; pins.removeAll(); positions.removeAll(); folder = nil
        directoryPosition = nil
        failure = nil
        cache.clear()
        if let previous { Task { await previous.close() } }
    }

    func setForeground(_ value: Bool) {
        guard foreground != value else { return }
        foreground = value
        revision &+= 1
        cancelOperation()
        if let directory { folder = (directory.workspace, directory.path) }
        directory = nil
        workspaces = []
        if value, let agent = selectedAgentID {
            let restoreFolder = folder, restoreRequest = request
            run { transport in
                let roots = try await transport.workspaces(agentID: agent)
                var restoredDirectory: FilesDirectory?
                if let (workspace, path) = restoreFolder {
                    let result = try await transport.directory(workspace: workspace, path: path)
                    guard result.workspace.workspaceID == workspace.workspaceID,
                          result.workspace.executionRootID == workspace.executionRootID,
                          result.path == path else { throw FilesFailure.invalidReference }
                    restoredDirectory = result
                }
                var download: FilesDownload?
                if let restoreRequest {
                    let source = try await self.source(for: restoreRequest, transport: transport)
                    download = try await self.download(source, transport: transport)
                }
                return {
                    self.workspaces = roots; self.directory = restoredDirectory
                    if let download, let restoreRequest { try self.store(download, request: restoreRequest) }
                }
            }
        }
    }

    func selectAgent(_ agentID: String?) {
        cancelOperation()
        selectedAgentID = agentID
        request = nil; pins.removeAll(); positions.removeAll(); folder = nil; directoryPosition = nil
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
        request = nil; directoryPosition = nil
        query = ""
        directory = nil
        run { transport in
            let result = try await transport.directory(workspace: workspace, path: path)
            guard result.workspace.workspaceID == workspace.workspaceID,
                  result.path == path,
                  workspace.executionRootID == nil || result.workspace.executionRootID == workspace.executionRootID else {
                throw FilesFailure.invalidReference
            }
            return { self.directory = result; self.folder = (result.workspace, result.path) }
        }
    }

    func openReference(_ reference: String) {
        openRequest(.source(.reference(reference)))
    }

    @discardableResult
    func openPlan(agentID: String, workID: String, plan: JSONValue) -> Bool {
        guard accepts(agentID), let locator = FilesPlanLocator(agentID: agentID, workID: workID, plan: plan) else {
            return rejectReference()
        }
        openRequest(.plan(locator))
        return true
    }

    @discardableResult
    func openArtifact(agentID: String, artifact: JSONValue) -> Bool {
        guard let request = artifactRequest(agentID: agentID, artifact: artifact) else { return false }
        openRequest(request)
        return true
    }

    func artifactRequest(agentID: String, artifact: JSONValue) -> FilesRequest? {
        guard accepts(agentID),
              string(artifact["owner_agent_id"]).map({ $0 == agentID }) ?? true,
              let reference = string(artifact["ref"]), !reference.isEmpty,
              reference.utf8.count <= 8192,
              !reference.hasPrefix("/"), !reference.hasPrefix("file:") else {
            _ = rejectReference(); return nil
        }
        return .source(.reference(reference))
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
        else { openRequest(.source(.workspace(directory.workspace, path: entry.path))) }
    }

    func dismissPreview() {
        request = nil
        cancelOperation()
        // Opening a plan can supersede the root-list request. Restore that list on return.
        if workspaces.isEmpty, let agentID = selectedAgentID { selectAgent(agentID) }
    }

    func openRequest(_ request: FilesRequest) {
        if case .plan(let locator) = request, !accepts(locator.owner) { _ = rejectReference(); return }
        self.request = request
        run { transport in
            let source = try await self.source(for: request, transport: transport)
            let download = try await self.download(source, transport: transport)
            return { try self.store(download, request: request) }
        }
    }

    private func source(for request: FilesRequest, transport: any FilesTransport) async throws -> FilesSource {
        if let pinned = pins[request] { return pinned }
        switch request {
        case .source(let source): return source
        case .plan(let locator):
            guard accepts(locator.owner) else { throw FilesFailure.forbidden }
            let candidates = try await transport.workspaces(agentID: locator.owner).filter {
                $0.workspaceID == locator.workspaceID &&
                    (locator.executionRootID == nil || $0.executionRootID == locator.executionRootID)
            }
            guard candidates.count == 1, var workspace = candidates.first else { throw FilesFailure.invalidReference }
            if workspace.executionRootID == nil {
                let snapshot = try await transport.directory(workspace: workspace, path: "")
                guard snapshot.workspace.workspaceID == locator.workspaceID, snapshot.path.isEmpty,
                      snapshot.workspace.executionRootID?.isEmpty == false else { throw FilesFailure.invalidReference }
                workspace = snapshot.workspace
            }
            guard workspace.executionRootID?.isEmpty == false else { throw FilesFailure.invalidReference }
            return .workspace(workspace, path: locator.path)
        }
    }

    private func download(_ source: FilesSource, transport: any FilesTransport) async throws -> FilesDownload {
        let capturedRevision = revision, capturedOperation = operation, authority = identity
        return try await transport.download(source: source, maximumBytes: FilesCache.maximumBytes) { [weak self] value in
            Task { @MainActor in
                guard let self, let authority, self.current(authority, capturedRevision, capturedOperation) else { return }
                self.progress = value
            }
        }
    }

    private func store(_ download: FilesDownload, request: FilesRequest) throws {
        prepared = try cache.prepare(download)
        preparedRequest = request
        if let location = download.location {
            if pins.count >= 16, pins[request] == nil { pins.removeValue(forKey: pins.keys.first!) }
            pins[request] = .workspace(FilesWorkspace(workspaceID: location.workspaceID,
                executionRootID: location.executionRootID, name: location.workspaceID), path: location.path)
        }
    }

    func readingPosition(for request: FilesRequest) -> FilesReadingPosition { positions[request] ?? .init() }
    func rememberPosition(_ value: FilesReadingPosition, for request: FilesRequest) {
        guard self.request == request, foreground else { return }
        if positions.count >= 16, positions[request] == nil { positions.removeValue(forKey: positions.keys.first!) }
        positions[request] = value
    }
    func cancelDownload() { cancelOperation(); cancelled = true }

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
        preparedRequest = nil; progress = nil; cancelled = false
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
            case 404: failure = .deleted
            case 410: failure = http.apiError?.code == "file_root_removed" ? .rootUnavailable : .deleted
            case 413: failure = .tooLarge
            case 415: failure = .unsupported
            default: failure = .unavailable
            }
        } else if error is URLError {
            failure = .offline
        } else if error is CancellationError {
            failure = .forbidden
        } else {
            failure = error as? FilesFailure ?? .unavailable
        }
    }
}
