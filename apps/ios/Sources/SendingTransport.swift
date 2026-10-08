import Foundation
import HolonClient

/// Credentials remain in the SDK transport, never in the durable store.
protocol SendingTransport: Sendable {
    func upload(agentID: String, attachment: SendingAttachment, file: URL) async throws -> SendingPreparedAttachment
    func send(agentID: String, requestID: UUID, payload: SendingPayload) async throws -> String?
    func models() async throws -> [SendingModel]
    func stop(agentID: String, runID: String) async throws
    func close() async
}

extension SendingTransport {
    func close() async {}
}

actor SendingClientTransport: SendingTransport {
    private let client: HolonClient
    private let authority: HolonConnectionIdentity
    private var bound: HolonConnectionIdentity?

    init(client: HolonClient, authority: HolonConnectionIdentity) {
        self.client = client
        self.authority = authority
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
        _ operation: @Sendable (HolonClient) async throws -> HolonResponse<T>
    ) async throws -> T {
        let identity = try await expected()
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
            _ = try await request { try await $0.setAgentModel(agentID: agentID, request: selection) }
        }
        let receipt = try await request { try await $0.sendOperatorPrompt(agentID: agentID, request: prompt) }
        guard receipt.isAccepted else { throw HolonClientError.malformedResponse }
        return receipt.messageID
    }

    func models() async throws -> [SendingModel] {
        let catalog = try await request { try await $0.modelCatalog() }
        guard case .array(let values) = catalog.availableModels else { return [] }
        return values.compactMap { value in
            if case .string(let id) = value { return SendingModel(id: id, name: id) }
            guard case .string(let id) = value["model_ref"] ?? value["id"] else { return nil }
            let name: String
            if case .string(let displayName) = value["display_name"] { name = displayName }
            else { name = id }
            return SendingModel(id: id, name: name)
        }
    }

    func stop(agentID: String, runID: String) async throws {
        _ = try await request { try await $0.stopCurrentRun(agentID: agentID, runID: runID) }
    }

    func close() async {
        await client.close()
    }
}
