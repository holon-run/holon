import Foundation
import HolonWire

public struct HolonServerLimits: Equatable, Sendable {
    public let promptBodyMaxBytes: Int
    public let promptFileAttachmentMaxBytes: Int
    public let promptImageAttachmentMaxBytes: Int
}

public struct HolonServerInfo: Equatable, Sendable {
    public let defaultAgentId: String
    public let authMode: String
    public let authRequired: Bool
    public let capabilities: Set<String>
    public let limits: HolonServerLimits?
}

public enum HolonCompatibility: Equatable, Sendable {
    case compatible(server: HolonServerInfo)
    case unsupportedProtocol(actualName: String, actualVersion: Int)
    case missingCapabilities(Set<String>)
    case rejectedHandshake
}

public struct HolonHandshake: Equatable, Sendable {
    public let ok: Bool
    public let protocolName: String
    public let protocolVersion: Int
    public let server: HolonServerInfo
    public let raw: JSONValue

    public init(data: Data) throws {
        let document = try WireDocument<HandshakeResponse>(data: data)
        let model = document.model
        ok = model.ok
        protocolName = model._protocol.name
        protocolVersion = model._protocol.version
        server = HolonServerInfo(
            defaultAgentId: model.runtime.defaultAgent,
            authMode: model.auth.mode,
            authRequired: model.auth._required,
            capabilities: Set(model.capabilities),
            limits: model.limits.map {
                HolonServerLimits(
                    promptBodyMaxBytes: $0.promptBodyMaxBytes,
                    promptFileAttachmentMaxBytes: $0.promptFileAttachmentMaxBytes,
                    promptImageAttachmentMaxBytes: $0.promptImageAttachmentMaxBytes)
            })
        raw = document.raw
    }

    public func checkCompatibility(requiredCapabilities: Set<String> = []) -> HolonCompatibility {
        guard ok else { return .rejectedHandshake }
        guard protocolName == "holon-control", protocolVersion == 1 else {
            return .unsupportedProtocol(actualName: protocolName, actualVersion: protocolVersion)
        }
        let missing = requiredCapabilities.subtracting(server.capabilities)
        guard missing.isEmpty else { return .missingCapabilities(missing) }
        return .compatible(server: server)
    }
}

/// The credential and raw payload are sensitive. Descriptions never include either.
public struct HolonSession: Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    public let ok: Bool
    public let userId: String
    public let expiresAt: Date?
    public let credential: String?
    public let raw: JSONValue

    public init(data: Data) throws {
        let document = try WireDocument<SessionResponse>(data: data)
        ok = document.model.ok
        userId = document.model.userId
        expiresAt = document.model.expiresAt
        credential = document.raw["credential"]?.stringValue
        raw = document.raw
    }

    public var description: String { "HolonSession(credential: <redacted>)" }
    public var debugDescription: String { description }
}

public struct HolonCurrentUser: Equatable, Sendable {
    public let ok: Bool
    public let userId: String
    public let displayName: String?
    public let authMethod: String
    public let raw: JSONValue

    public init(data: Data) throws {
        let document = try WireDocument<CurrentUserPayload>(data: data)
        ok = document.model.ok
        userId = document.model.userId
        displayName = document.model.displayName
        authMethod = document.model.authMethod
        raw = document.raw
    }
}

private struct CurrentUserPayload: Decodable {
    let ok: Bool
    let userId: String
    let displayName: String?
    let authMethod: String

    enum CodingKeys: String, CodingKey {
        case ok
        case userId = "user_id"
        case displayName = "display_name"
        case authMethod = "auth_method"
    }
}

public struct HolonAgentSummary: Equatable, Sendable {
    public let id: String
    public let displayName: String
    public let isDefault: Bool
    public let registryStatus: String
    public let runtimeStatus: String
    public let effectiveModel: String
    public let modelSource: String
    public let overrideModel: String?
    public let overrideReasoningEffort: String?
    public let pending: Int
    public let currentRunId: String?
    public let schedulingPosture: String
    public let postureReason: String?
    public let waitingReason: String?
    public let currentWorkItemId: String?
    public let workspaceLabel: String?
    public let workspaceId: String?
    public let executionRootId: String?
    public let workspaceProjectionKind: String?
    public let workspaceProjectionMetadata: JSONValue?
    public let raw: JSONValue

    fileprivate init(model: AgentListEntry, raw: JSONValue) {
        id = model.identity.agentId
        displayName = model.identity.name ?? model.identity.agentId
        isDefault = model.identity.isDefaultAgent
        registryStatus = raw["identity"]?["status"]?.stringValue ?? model.identity.status.rawValue
        runtimeStatus = raw["status"]?.stringValue ?? model.status.rawValue
        effectiveModel = model.model.effectiveModel
        modelSource = raw["model"]?["source"]?.stringValue ?? model.model.source.rawValue
        overrideModel = model.model.overrideModel
        overrideReasoningEffort = model.model.overrideReasoningEffort
        pending = model.pending ?? 0
        currentRunId = model.currentRunId
        schedulingPosture = raw["scheduling_posture"]?["posture"]?.stringValue ?? "unknown"
        postureReason = model.schedulingPosture?.reason
        waitingReason = raw["waiting_reason"]?.stringValue
        currentWorkItemId = model.schedulingPosture?.workItemId
        workspaceLabel = model.activeWorkspaceEntry?.workspaceAnchor
        workspaceId = model.activeWorkspaceEntry?.workspaceId
        executionRootId = model.activeWorkspaceEntry?.executionRootId
        workspaceProjectionKind = raw["active_workspace_entry"]?["projection_kind"]?.stringValue
        workspaceProjectionMetadata = raw["active_workspace_entry"]?["projection_metadata"]
        self.raw = raw
    }
}

public struct HolonRoster: Equatable, Sendable {
    public let agents: [HolonAgentSummary]
    public let raw: JSONValue

    public init(data: Data) throws {
        let document = try WireDocument<AgentListResponse>(data: data)
        guard case .array(let entries) = document.raw else {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "Expected roster array"))
        }
        agents = zip(document.model, entries).map { HolonAgentSummary(model: $0.0, raw: $0.1) }
        raw = document.raw
    }
}

public struct HolonAPIError: Error, Equatable, Sendable {
    public let code: String
    public let message: String
    public let retryable: Bool
    public let domain: String?
    public let context: [String: String]
    public let detail: String?
    public let hint: String?
    public let correlation: JSONValue?
    public let extensions: [String: JSONValue]
    public let raw: JSONValue

    public init(data: Data) throws {
        let document = try WireDocument<ModelErrorResponse>(data: data)
        code = document.model.code
        message = document.model.error
        retryable = document.model.retryable ?? false
        domain = document.raw["domain"]?.stringValue
        context = document.model.context ?? [:]
        detail = context["detail"]
        hint = document.model.hint
        correlation = document.raw["correlation"]
        let known: Set<String> = ["ok", "code", "error", "retryable", "domain", "context", "hint", "correlation"]
        if case .object(let fields) = document.raw {
            extensions = fields.filter { !known.contains($0.key) }
        } else {
            extensions = [:]
        }
        raw = document.raw
    }

    public func requiresSessionRenewal(statusCode: Int) -> Bool {
        Self.requiresSessionRenewal(statusCode: statusCode, code: code)
    }

    public static func requiresSessionRenewal(statusCode: Int, code: String?) -> Bool {
        guard statusCode == 401 else { return false }
        guard let code else { return true }
        return [
            "auth_required", "invalid_static_token", "pairing_invalid_or_expired",
            "session_invalid_or_expired", "session_expired_or_revoked", "session_user_disabled",
        ].contains(code)
    }
}

private extension JSONValue {
    var stringValue: String? {
        guard case .string(let value) = self else { return nil }
        return value
    }
}
