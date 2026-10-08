import Foundation

/// Passive bytes, never an executable local path or credential-bearing URL.
public struct HolonDownloadedArtifact: Sendable {
    public let data: Data
    public let mediaType: String
}

public struct HolonDownloadProgress: Equatable, Sendable {
    public let receivedBytes: Int
    public let totalBytes: Int?
    public init(receivedBytes: Int, totalBytes: Int?) {
        self.receivedBytes = receivedBytes; self.totalBytes = totalBytes
    }
}

/// The complete server-issued base identity. Relative paths are resolved by the server, not joined locally.
public struct HolonFileLocation: Hashable, Sendable {
    public let workspaceID: String
    public let executionRootID: String
    public let path: String
    public let absolutePath: String
    public let rootKind: String

    public init(raw: JSONValue) throws {
        func text(_ key: String) -> String? {
            guard case .string(let value) = raw[key], !value.isEmpty,
                  value.utf8.count <= 16_384, !value.contains("\0") else { return nil }
            return value
        }
        guard let workspace = text("workspace_id"), let root = text("execution_root_id"),
              let path = text("path"), !path.hasPrefix("/"), !path.contains("\\"),
              path.split(separator: "/", omittingEmptySubsequences: false).allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }),
              let absolute = text("absolute_path"), absolute.hasPrefix("/"),
              raw["kind"] == .string("file"), let kind = text("root_kind"),
              ["canonical_root", "git_worktree_root"].contains(kind) else { throw HolonClientError.malformedResponse }
        workspaceID = workspace; executionRootID = root; self.path = path; absolutePath = absolute; rootKind = kind
    }

    public var payload: JSONValue {
        .object(["workspace_id": .string(workspaceID), "execution_root_id": .string(executionRootID),
                 "path": .string(path), "absolute_path": .string(absolutePath), "kind": .string("file"),
                 "root_kind": .string(rootKind)])
    }
}

public enum HolonFileReference: Sendable {
    case absolutePath(String)
    case workspaceURI(String)
    case relativePath(String, baseFile: HolonFileLocation)

    var payload: JSONValue {
        switch self {
        case .absolutePath(let path):
            return .object(["type": .string("absolute_path"), "absolute_path": .string(path)])
        case .workspaceURI(let uri):
            return .object(["type": .string("workspace_uri"), "workspace_uri": .string(uri)])
        case .relativePath(let path, let base):
            return .object(["type": .string("relative_path"), "relative_path": .string(path), "base_file": base.payload])
        }
    }
}

extension HolonClient {
    // Open JSON retains unknown states, authoritative owner, brief and plan metadata.
    public func workItems(agentID: String, limit: Int = 50) async throws -> HolonResponse<JSONValue> {
        guard limit > 0 else { throw HolonClientError.invalidRequest }
        return try await getJSON(path: ["agents", agentID, "work-items"], query: ["limit": String(limit)])
    }

    public func workItem(agentID: String, workItemID: String) async throws -> HolonResponse<JSONValue> {
        try await getJSON(path: ["agents", agentID, "work-items", workItemID])
    }

    public func tasks(agentID: String, limit: Int = 50) async throws -> HolonResponse<JSONValue> {
        guard limit > 0 else { throw HolonClientError.invalidRequest }
        return try await getJSON(path: ["agents", agentID, "tasks"], query: ["limit": String(limit)])
    }

    public func task(agentID: String, taskID: String) async throws -> HolonResponse<JSONValue> {
        try await getJSON(path: ["agents", agentID, "tasks", taskID])
    }

    /// Nonblocking server-bounded output with its original truncation markers.
    public func taskOutput(agentID: String, taskID: String) async throws -> HolonResponse<JSONValue> {
        try await getJSON(path: ["agents", agentID, "tasks", taskID, "output"], query: ["block": "false"])
    }

    /// Workspace snapshot includes active projection and execution-root information.
    public func agentWorkspaces(agentID: String) async throws -> HolonResponse<JSONValue> {
        let response = try await getJSON(path: ["agents", agentID, "state"])
        guard let workspace = response.value["workspace"],
              case .array = workspace["workspaces"] else { throw HolonClientError.malformedResponse }
        return HolonResponse(identity: response.identity, value: workspace)
    }

    public func browseWorkspaceDirectory(workspaceID: String, path: String = "",
                                         executionRootID: String? = nil)
        async throws -> HolonResponse<JSONValue> {
        let response = try await getJSON(path: Self.workspaceFilePath(workspaceID, path),
                                        query: Self.rootQuery(executionRootID))
        guard response.value["type"] == .string("directory") else { throw HolonClientError.malformedResponse }
        return response
    }

    public func downloadWorkspaceFile(workspaceID: String, path: String, executionRootID: String? = nil,
                                      maximumBytes: Int = 16_777_216,
                                      allowedContentTypes: Set<String> = ["application/octet-stream", "text/plain",
                                          "text/markdown", "application/pdf", "image/png", "image/jpeg", "image/webp"],
                                      progress: (@Sendable (HolonDownloadProgress) -> Void)? = nil)
        async throws -> HolonResponse<HolonDownloadedArtifact> {
        guard !path.isEmpty else { throw HolonClientError.invalidRequest }
        var query = Self.rootQuery(executionRootID)
        query["download"] = "true"
        return try await downloadBinary(path: Self.workspaceFilePath(workspaceID, path), query: query,
                                        maximumBytes: maximumBytes, allowedContentTypes: allowedContentTypes, progress: progress)
    }

    public func workspaceFileMetadata(workspaceID: String, path: String, executionRootID: String? = nil)
        async throws -> HolonResponse<JSONValue> {
        guard !path.isEmpty else { throw HolonClientError.invalidRequest }
        var query = Self.rootQuery(executionRootID)
        query["meta"] = "true"
        let response = try await getJSON(path: Self.workspaceFilePath(workspaceID, path), query: query)
        guard response.value["type"] == .string("file") else { throw HolonClientError.malformedResponse }
        return response
    }

    public func downloadWorkspaceArtifact(locator: String, maximumBytes: Int = 16_777_216,
                                          allowedContentTypes: Set<String> = ["application/octet-stream", "text/plain",
                                              "text/markdown", "application/pdf", "image/png", "image/jpeg", "image/webp"])
        async throws -> HolonResponse<HolonDownloadedArtifact> {
        guard let parts = URLComponents(string: locator), parts.scheme == "workspace",
              let workspace = parts.host, !workspace.isEmpty,
              parts.user == nil, parts.password == nil, parts.port == nil, parts.fragment == nil,
              parts.path.hasPrefix("/"),
              parts.queryItems?.allSatisfy({ $0.name == "root" && $0.value?.isEmpty == false }) ?? true,
              (parts.queryItems?.count ?? 0) <= 1 else { throw HolonClientError.invalidRequest }
        return try await downloadWorkspaceFile(workspaceID: workspace,
            path: String(parts.path.dropFirst()), executionRootID: parts.queryItems?.first?.value,
            maximumBytes: maximumBytes, allowedContentTypes: allowedContentTypes)
    }

    private static func rootQuery(_ root: String?) -> [String: String] {
        root.map { ["execution_root_id": $0] } ?? [:]
    }

    private static func workspaceFilePath(_ workspace: String, _ path: String) throws -> [String] {
        guard !path.hasPrefix("/"), !path.contains("\\"), !path.contains("\0") else {
            throw HolonClientError.invalidRequest
        }
        let segments = path.isEmpty ? [] : path.components(separatedBy: "/")
        guard segments.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
            throw HolonClientError.invalidRequest
        }
        return ["workspaces", workspace, "files"] + segments
    }
}
