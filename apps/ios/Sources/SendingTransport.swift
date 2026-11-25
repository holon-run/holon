import Foundation
import HolonClient

/// Credentials remain in the SDK transport, never in the durable store.
protocol SendingTransport: Sendable {
    func upload(agentID: String, attachment: SendingAttachment, file: URL) async throws -> SendingPreparedAttachment
    func send(agentID: String, requestID: UUID, payload: SendingPayload) async throws -> String?
    func models() async throws -> [SendingModel]
    func agentModel(agentID: String) async throws -> AgentModelConfiguration
    func updateAgentModel(agentID: String, selection: AgentModelSelection) async throws -> AgentModelConfiguration
    func stop(agentID: String, runID: String) async throws
    func close() async
}

extension SendingTransport {
    func close() async {}
    func agentModel(agentID: String) async throws -> AgentModelConfiguration { throw SendingFailure.unavailable }
    func updateAgentModel(agentID: String, selection: AgentModelSelection) async throws -> AgentModelConfiguration {
        throw SendingFailure.unavailable
    }
}

actor SendingClientTransport: SendingTransport {
    private let client: HolonClient
    private let authority: HolonConnectionIdentity
    private let consent: SharingConsent
    private let apiBaseURL: URL
    private var bound: HolonConnectionIdentity?

    init(client: HolonClient, authority: HolonConnectionIdentity,
         apiBaseURL: URL, consent: SharingConsent = .shared) {
        self.client = client
        self.authority = authority
        self.apiBaseURL = apiBaseURL
        self.consent = consent
    }

    private func expected() async throws -> HolonConnectionIdentity {
        let current = await client.identity
        guard authority.runtimeID != nil, authority.userID != nil, authority.visibilityScopeID != nil,
              current.networkID == authority.networkID, current.runtimeID == authority.runtimeID,
              current.userID == authority.userID, current.visibilityScopeID == authority.visibilityScopeID,
              bound == nil || bound == current else { throw CancellationError() }
        bound = current
        return current
    }

    private func request<T: Sendable>(
        sharing: Bool = false,
        _ operation: @Sendable (HolonClient) async throws -> HolonResponse<T>
    ) async throws -> T {
        let identity = try await expected()
        if sharing { try consent.require(apiBaseURL) }
        do {
            let response = try await operation(client)
            guard response.identity == identity, try await expected() == identity else { throw CancellationError() }
            return response.value
        } catch let failure as HolonHTTPFailure {
            guard failure.identity == identity, try await expected() == identity else { throw CancellationError() }
            if failure.statusCode == 401 || failure.statusCode == 403 {
                throw HolonHTTPFailure(statusCode: failure.statusCode, apiError: failure.apiError, identity: authority)
            }
            if (400..<500).contains(failure.statusCode), failure.statusCode != 408, failure.statusCode != 429 {
                throw SendingFailure.rejected("Server rejected the request (HTTP \(failure.statusCode)).")
            }
            throw failure
        }
    }

    func upload(agentID: String, attachment: SendingAttachment, file: URL) async throws -> SendingPreparedAttachment {
        _ = try await expected()
        try consent.require(apiBaseURL)
        let data = try Data(contentsOf: file)
        guard !data.isEmpty, data.count <= HolonPromptLimits.maximumAttachmentBytes else {
            throw SendingFailure.oversizedAttachment(attachment.name)
        }
        // Preparation is persisted before POST; retry never rereads a changed source file.
        return SendingPreparedAttachment(name: attachment.name, contentType: attachment.contentType, data: data)
    }

    func send(agentID: String, requestID: UUID, payload: SendingPayload) async throws -> String? {
        let prompt: HolonPromptRequest
        do {
            let attachments = try payload.attachments.map {
                try HolonPromptAttachment(kind: ["image/png", "image/jpeg", "image/jpg", "image/gif", "image/webp"].contains($0.contentType) ? .image : .file,
                                          name: $0.name, mediaType: $0.contentType, data: $0.data)
            }
            prompt = try HolonPromptRequest(clientRequestID: requestID.uuidString, text: payload.text,
                                           attachments: attachments)
        } catch { throw SendingFailure.rejected("Invalid prompt or attachment body exceeds the server limit.") }
        if let model = payload.modelID {
            let selection = try HolonAgentModelRequest(model: model)
            _ = try await request(sharing: true) { try await $0.setAgentModel(agentID: agentID, request: selection) }
        }
        let receipt = try await request(sharing: true) { try await $0.sendOperatorPrompt(agentID: agentID, request: prompt) }
        guard receipt.isAccepted else { throw HolonClientError.malformedResponse }
        return receipt.messageID
    }

    func models() async throws -> [SendingModel] {
        let catalog = try await request { try await $0.modelCatalog() }
        return Self.modelChoices(catalog.raw)
    }

    nonisolated static func modelChoices(_ catalog: JSONValue) -> [SendingModel] {
        func efforts(_ value: JSONValue?) -> [String] {
            guard case .array(let list) = value else { return [] }
            var seen: Set<String> = []
            return list.compactMap(\.readingString).filter { !$0.isEmpty && seen.insert($0).inserted }
        }
        var models: [String: SendingModel] = [:]
        if case .array(let availability) = catalog["model_availability"] {
            // The metadata catalog includes built-ins for providers this server cannot call.
            for item in availability {
                guard item["available"] == .bool(true)
                    || (item["available"] == .bool(false) && item["failure_disposition"]?.readingString == "retryable"),
                    let model = item["model"]?.readingString, !model.isEmpty else { continue }
                let parts = model.split(separator: "/", maxSplits: 1)
                let id: String
                if parts.count == 2, !parts[0].contains("@"),
                   let family = item["provider_family"]?.readingString,
                   let endpoint = item["endpoint"]?.readingString {
                    id = "\(family)@\(endpoint)/\(parts[1])"
                } else {
                    id = model
                }
                models[id] = SendingModel(id: id, name: item["display_name"]?.readingString ?? model,
                                          reasoningEfforts: efforts(item["policy"]?["reasoning_effort_options"]))
            }
        } else if case .array(let values) = catalog["available_models"] {
            for value in values {
                guard let id = value.readingString ?? (value["model"] ?? value["model_ref"] ?? value["id"])?.readingString,
                      !id.isEmpty else { continue }
                models[id] = SendingModel(id: id, name: value["display_name"]?.readingString ?? id,
                                          reasoningEfforts: efforts(value["reasoning_effort_options"]
                                                                    ?? value["policy"]?["reasoning_effort_options"]))
            }
        }
        return models.values.sorted { ($0.name.lowercased(), $0.id) < ($1.name.lowercased(), $1.id) }
    }

    func agentModel(agentID: String) async throws -> AgentModelConfiguration {
        try Self.modelConfiguration(await request { try await $0.agentModel(agentID: agentID) })
    }

    func updateAgentModel(agentID: String, selection: AgentModelSelection) async throws -> AgentModelConfiguration {
        let state: HolonAgentModelState
        switch selection {
        case .automatic:
            state = try await request(sharing: true) { try await $0.clearAgentModel(agentID: agentID) }
        case .manual(let model, let effort, let tier):
            let body = try HolonAgentModelRequest(model: model, reasoningEffort: effort, serviceTier: tier)
            state = try await request(sharing: true) { try await $0.setAgentModel(agentID: agentID, request: body) }
        }
        return try Self.modelConfiguration(state)
    }

    private static func modelConfiguration(_ state: HolonAgentModelState) throws -> AgentModelConfiguration {
        let raw = state.raw
        guard let effective = raw["effective_model"]?.readingString, !effective.isEmpty else {
            throw HolonClientError.malformedResponse
        }
        return AgentModelConfiguration(effectiveModel: effective,
                                       activeModel: raw["active_model"]?.readingString,
                                       source: raw["source"]?.readingString,
                                       overrideModel: raw["override_model"]?.readingString,
                                       reasoningEffort: raw["override_reasoning_effort"]?.readingString,
                                       serviceTier: raw["override_service_tier"]?.readingString)
    }

    func stop(agentID: String, runID: String) async throws {
        _ = try await request { try await $0.stopCurrentRun(agentID: agentID, runID: runID) }
    }

    func close() async {
        await client.close()
    }
}
