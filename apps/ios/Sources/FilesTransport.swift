import Foundation
import HolonClient

protocol FilesTransport: Sendable {
    func validate() async throws
    func workspaces(agentID: String) async throws -> [FilesWorkspace]
    func directory(workspace: FilesWorkspace, path: String) async throws -> FilesDirectory
    func download(source: FilesSource, maximumBytes: Int) async throws -> FilesDownload
    func download(source: FilesSource, maximumBytes: Int,
                  progress: (@Sendable (HolonDownloadProgress) -> Void)?) async throws -> FilesDownload
    func close() async
}

extension FilesTransport {
    func download(source: FilesSource, maximumBytes: Int,
                  progress: (@Sendable (HolonDownloadProgress) -> Void)?) async throws -> FilesDownload {
        try await download(source: source, maximumBytes: maximumBytes)
    }
}

/// This independent SDK client never adopts a rebound generation, including a same-user rebind.
actor FilesClientTransport: FilesTransport {
    private let client: HolonClient
    private let authority: HolonConnectionIdentity
    private var bound: HolonConnectionIdentity?
    private var closed = false
    private static let contentTypes: Set<String> = [
        "application/octet-stream", "application/pdf", "application/json", "application/xml",
        "application/javascript", "text/plain", "text/markdown", "text/x-markdown",
        "text/html", "text/css", "text/javascript", "text/xml", "text/csv",
        "image/png", "image/jpeg", "image/webp", "image/gif", "image/heic", "image/svg+xml"
    ]

    init(client: HolonClient, authority: HolonConnectionIdentity) {
        self.client = client
        self.authority = authority
    }

    private func expected() async throws -> HolonConnectionIdentity {
        let current = await client.identity
        guard !closed, authority.runtimeID?.isEmpty == false,
              authority.userID?.isEmpty == false, authority.visibilityScopeID?.isEmpty == false,
              current.networkID == authority.networkID, current.runtimeID == authority.runtimeID,
              current.userID == authority.userID, current.visibilityScopeID == authority.visibilityScopeID,
              bound == nil || bound == current else { throw CancellationError() }
        bound = current
        return current
    }

    func validate() async throws { _ = try await expected() }

    private func request<T: Sendable>(
        _ body: @Sendable (HolonClient) async throws -> HolonResponse<T>
    ) async throws -> T {
        let identity = try await expected()
        try Task.checkCancellation()
        do {
            let response = try await body(client)
            guard response.identity == identity, try await expected() == identity else { throw CancellationError() }
            try Task.checkCancellation()
            return response.value
        } catch let http as HolonHTTPFailure {
            guard http.identity == identity, try await expected() == identity else { throw CancellationError() }
            throw HolonHTTPFailure(statusCode: http.statusCode, apiError: http.apiError, identity: authority)
        }
    }

    func workspaces(agentID: String) async throws -> [FilesWorkspace] {
        let raw = try await request { try await $0.agentWorkspaces(agentID: agentID) }
        guard case .array(let values) = raw["workspaces"], values.count <= 256 else {
            throw FilesFailure.unavailable
        }
        return try values.map {
            guard let id = $0["workspace_id"]?.filesString, !id.isEmpty else { throw FilesFailure.unavailable }
            return FilesWorkspace(workspaceID: id, executionRootID: $0["execution_root_id"]?.filesString,
                                  name: $0["repo_name"]?.filesString ?? $0["workspace_alias"]?.filesString ?? id)
        }
    }

    func directory(workspace: FilesWorkspace, path: String) async throws -> FilesDirectory {
        let raw = try await request {
            try await $0.browseWorkspaceDirectory(workspaceID: workspace.workspaceID, path: path,
                                                executionRootID: workspace.executionRootID)
        }
        guard raw["type"] == .string("directory"),
              raw["workspace_id"]?.filesString == workspace.workspaceID,
              let returnedRoot = raw["execution_root_id"]?.filesString, !returnedRoot.isEmpty,
              workspace.executionRootID == nil || returnedRoot == workspace.executionRootID,
              let directoryPath = raw["path"]?.filesString, directoryPath == path,
              case .array(let values) = raw["entries"], values.count <= 10_000 else {
            throw FilesFailure.invalidReference
        }
        let entries = try values.map { entry in
            guard let name = entry["name"]?.filesString, !name.isEmpty,
                  !name.contains("/"), !name.contains("\\"), !name.contains("\0"),
                  name != ".", name != ".." else { throw FilesFailure.invalidReference }
            return FilesEntry(name: name, path: path.isEmpty ? name : path + "/" + name,
                              isDirectory: entry["type"] == .string("directory"),
                              size: entry["size"]?.readingInteger.flatMap { $0 >= 0 ? $0 : nil },
                              modified: entry["modified"]?.readingInteger.flatMap { $0 >= 0 ? Date(timeIntervalSince1970: Double($0)) : nil },
                              mediaType: entry["mime_type"]?.filesString)
        }
        let resolvedWorkspace = FilesWorkspace(workspaceID: workspace.workspaceID,
                                              executionRootID: returnedRoot, name: workspace.name)
        return FilesDirectory(workspace: resolvedWorkspace, path: path, entries: entries)
    }

    func download(source: FilesSource, maximumBytes: Int) async throws -> FilesDownload {
        try await download(source: source, maximumBytes: maximumBytes, progress: nil)
    }

    func download(source: FilesSource, maximumBytes: Int,
                  progress: (@Sendable (HolonDownloadProgress) -> Void)?) async throws -> FilesDownload {
        let workspace: FilesWorkspace
        let path: String
        switch source {
        case .workspace(let target, let relative):
            workspace = target
            path = relative
        case .reference(let reference):
            guard reference.utf8.count <= 16_384,
                  reference.hasPrefix("/") || reference.hasPrefix("workspace://") else {
                throw FilesFailure.invalidReference
            }
            let input: HolonFileReference = reference.hasPrefix("/") ? .absolutePath(reference) : .workspaceURI(reference)
            let raw = try await request { try await $0.resolveFileReference(input) }
            (workspace, path) = try Self.resolved(raw)
        case .relative(let relative, let base):
            let raw = try await request { try await $0.resolveFileReference(.relativePath(relative, baseFile: base)) }
            (workspace, path) = try Self.resolved(raw)
        }
        do {
            let metadata = try await request {
                try await $0.workspaceFileMetadata(workspaceID: workspace.workspaceID, path: path,
                                                  executionRootID: workspace.executionRootID)
            }
            let location = try HolonFileLocation(raw: metadata)
            guard location.workspaceID == workspace.workspaceID, location.path == path,
                  workspace.executionRootID == nil || location.executionRootID == workspace.executionRootID else {
                throw FilesFailure.invalidReference
            }
            if let size = metadata["size"]?.readingInteger, size > Int64(maximumBytes) { throw FilesFailure.tooLarge }
            let artifact = try await request {
                try await $0.downloadWorkspaceFile(workspaceID: workspace.workspaceID, path: path,
                    executionRootID: location.executionRootID, maximumBytes: maximumBytes,
                    allowedContentTypes: Self.contentTypes, progress: progress)
            }
            guard artifact.data.count <= maximumBytes else { throw FilesFailure.tooLarge }
            return FilesDownload(data: artifact.data, mediaType: artifact.mediaType,
                                 name: (path as NSString).lastPathComponent, location: location)
        } catch HolonClientError.streamLimitExceeded {
            throw FilesFailure.tooLarge
        } catch HolonClientError.unexpectedContentType {
            throw FilesFailure.unsupported
        }
    }

    static func resolved(_ raw: JSONValue) throws -> (FilesWorkspace, String) {
        guard case .array(let results) = raw["results"], results.count == 1 else {
            throw FilesFailure.invalidReference
        }
        let result = results[0]
        guard result["status"] == .string("resolved") else {
            switch result["reason"]?.filesString {
            case "forbidden", "permission_denied", "not_authorized": throw FilesFailure.forbidden
            case "execution_root_not_found", "root_removed": throw FilesFailure.rootUnavailable
            case "not_found", "missing", "deleted": throw FilesFailure.deleted
            case "unsupported", "unsupported_reference": throw FilesFailure.unsupported
            default: throw FilesFailure.invalidReference
            }
        }
        guard let location = result["location"],
              let id = location["workspace_id"]?.filesString, !id.isEmpty,
              let root = location["execution_root_id"]?.filesString, !root.isEmpty,
              let path = location["path"]?.filesString, !path.isEmpty,
              location["kind"] == .string("file") else { throw FilesFailure.invalidReference }
        return (FilesWorkspace(workspaceID: id, executionRootID: root, name: id), path)
    }

    func close() async {
        closed = true
        await client.close()
    }
}

private extension JSONValue {
    var filesString: String? {
        if case .string(let value) = self { return value }
        return nil
    }
}
