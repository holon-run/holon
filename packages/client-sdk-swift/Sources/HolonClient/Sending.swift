import Foundation

/// Limits of the current HTTP control prompt route, not negotiated upload limits.
public enum HolonPromptLimits {
    public static let maximumAttachmentBytes = 20 * 1024 * 1024
    public static let maximumBodyBytes = 32 * 1024 * 1024
    public static let maximumRequestIDBytes = 200
}

/// Attachments upload inline with the prompt; there is no separate upload endpoint.
public struct HolonPromptAttachment: Sendable, Equatable {
    public enum Kind: String, Sendable { case image, file }
    public let raw: JSONValue

    public init(kind: Kind, name: String? = nil, mediaType: String, data: Data) throws {
        guard !data.isEmpty, data.count <= HolonPromptLimits.maximumAttachmentBytes,
              !mediaType.isEmpty,
              kind != .image || ["image/png", "image/jpeg", "image/jpg", "image/gif", "image/webp"]
                .contains(mediaType) else { throw HolonClientError.invalidRequest }
        var fields: [String: JSONValue] = [
            "kind": .string(kind.rawValue), "media_type": .string(mediaType),
            "data_base64": .string(data.base64EncodedString())]
        if let name { fields["name"] = .string(name) }
        raw = .object(fields)
    }
}

/// Persist this immutable value before sending. Reuse it after an unknown outcome.
public struct HolonPromptRequest: Sendable, Equatable {
    public let clientRequestID: String
    public let payload: JSONValue

    public init(clientRequestID: String, text: String,
                attachments: [HolonPromptAttachment] = [], workItemID: String? = nil) throws {
        guard !clientRequestID.isEmpty,
              clientRequestID == clientRequestID.trimmingCharacters(in: .whitespacesAndNewlines),
              clientRequestID.utf8.count <= HolonPromptLimits.maximumRequestIDBytes,
              !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty
        else { throw HolonClientError.invalidRequest }
        self.clientRequestID = clientRequestID
        var fields: [String: JSONValue] = [
            "client_request_id": .string(clientRequestID), "text": .string(text),
            "attachments": .array(attachments.map(\.raw))]
        if let workItemID { fields["work_item_id"] = .string(workItemID) }
        payload = .object(fields)
        guard try JSONEncoder().encode(payload).count <= HolonPromptLimits.maximumBodyBytes
        else { throw HolonClientError.invalidRequest }
    }
}

public struct HolonPromptReceipt: Sendable, Equatable {
    public let agentID: String
    public let messageID: String
    /// Unknown future dispositions remain intact; callers must not assume acceptance.
    public let disposition: String
    public let raw: JSONValue
    public var isAccepted: Bool { disposition == "accepted" || disposition == "duplicate" }

    init(raw: JSONValue, agentID: String) throws {
        guard raw["ok"] == .bool(true), raw["agent_id"] == .string(agentID),
              case .string(let messageID) = raw["message_id"], !messageID.isEmpty,
              case .string(let disposition) = raw["disposition"], !disposition.isEmpty
        else { throw HolonClientError.malformedResponse }
        self.agentID = agentID
        self.messageID = messageID
        self.disposition = disposition
        self.raw = raw
    }
}

public struct HolonAgentModelRequest: Sendable, Equatable {
    public let payload: JSONValue
    public init(model: String, reasoningEffort: String? = nil) throws {
        guard !model.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { throw HolonClientError.invalidRequest }
        var fields: [String: JSONValue] = ["model": .string(model),
                                          "authority_class": .string("operator_instruction")]
        if let reasoningEffort { fields["reasoning_effort"] = .string(reasoningEffort) }
        payload = .object(fields)
    }
}

public struct HolonModelCatalog: Sendable {
    public let raw: JSONValue
    public var availableModels: JSONValue { raw["available_models"]! }
    init(raw: JSONValue) throws {
        guard case .array = raw["available_models"] else { throw HolonClientError.malformedResponse }
        self.raw = raw
    }
}

public struct HolonAgentModelState: Sendable {
    public let raw: JSONValue
    public var effectiveModel: JSONValue? { raw["effective_model"] }
    init(raw: JSONValue) throws {
        guard case .object = raw else { throw HolonClientError.malformedResponse }
        self.raw = raw
    }
}

public struct HolonRunStopReceipt: Sendable {
    public let raw: JSONValue
    public let runID: String
    init(raw: JSONValue, agentID: String, runID: String) throws {
        guard raw["ok"] == .bool(true), raw["aborted"] == .bool(true),
              raw["agent_id"] == .string(agentID), raw["run_id"] == .string(runID),
              raw["mode"] == .string("stop_after_abort")
        else { throw HolonClientError.malformedResponse }
        self.raw = raw
        self.runID = runID
    }
}
