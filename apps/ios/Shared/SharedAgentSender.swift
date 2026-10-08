import Foundation
import HolonClient
import UniformTypeIdentifiers

struct SharedAgent: Identifiable, Equatable, Sendable { let id: String; let name: String }

/// Foreground-only extension transport. Every confirmation reboots authoritative scope.
actor SharedAgentSender {
    private let session: SharedSession
    private let vault: SharedSessionVault
    private let client: HolonClient
    init(session: SharedSession, vault: SharedSessionVault) throws {
        self.session = session; self.vault = vault
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 20
        configuration.timeoutIntervalForResource = 30
        client = try HolonClient(endpoint: HolonEndpoint(apiBaseURL: session.apiBaseURL,
            allowInsecureHTTP: session.allowInsecureHTTP), networkID: session.networkID,
            credential: session.credential.isEmpty ? nil : session.credential,
            configuration: configuration, maximumResponseBytes: 4_194_304)
    }
    func agents() async throws -> [SharedAgent] {
        try vault.require(session)
        let me = try await client.currentUser().value
        guard me.ok, me.userId == session.userID else { throw SharedShareError.changedConnection }
        try vault.require(session)
        let handshake = try await client.handshake().value
        guard case .compatible = handshake.checkCompatibility(requiredCapabilities: ["control.prompt-idempotency.v1", "control.prompt-attachments.v1"]) else {
            throw SharedShareError.incompatible
        }
        let raw = try await client.getJSON(path: ["agents", "snapshot"]).value
        try vault.require(session)
        guard raw["runtime_id"] == .string(session.runtimeID),
              raw["visibility_scope_id"] == .string(session.visibilityScopeID),
              case .array(let entries) = raw["agents"] else { throw SharedShareError.changedConnection }
        let agents = try entries.map { entry -> SharedAgent in
            guard case .string(let id) = entry["agent"]?["identity"]?["agent_id"],
                  !id.isEmpty, id.utf8.count <= 512 else { throw HolonClientError.malformedResponse }
            let name: String
            if case .string(let value) = entry["agent"]?["identity"]?["name"] { name = value } else { name = id }
            return SharedAgent(id: id, name: name)
        }
        guard Set(agents.map(\.id)).count == agents.count else { throw HolonClientError.malformedResponse }
        return agents.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }
    func send(payloadID: UUID, agentID: String, store: SharedImportStore) async throws {
        guard try await agents().contains(where: { $0.id == agentID }) else { throw SharedShareError.missingAgent }
        let target = SharedShareTarget(session: session, agentID: agentID)
        let payload = try store.prepareDelivery(id: payloadID, target: target)
        let prompt = try Self.prompt(payload, store: store)
        try vault.require(session)
        // Durable unknown precedes POST, including termination before the reply arrives.
        try store.markDelivery(id: payloadID, target: target, state: .unknown)
        try vault.require(session)
        let receipt = try await client.sendOperatorPrompt(agentID: agentID, request: prompt).value
        guard receipt.isAccepted else { throw HolonClientError.malformedResponse }
        // A changed host identity does not erase a receipt for an already admitted request.
        try store.markDelivery(id: payloadID, target: target, state: .accepted)
        try? store.consume(id: payloadID, enqueueSucceeded: true)
    }
    static func prompt(_ payload: SharedImportPayload, store: SharedImportStore) throws -> HolonPromptRequest {
        let attachments = try payload.attachments.map { item in
            let mime = UTType(item.typeIdentifier)?.preferredMIMEType ?? "application/octet-stream"
            // Unsupported native image formats remain file attachments, never forged PNG.
            let imageTypes = ["image/png", "image/jpeg", "image/jpg", "image/gif", "image/webp"]
            return try HolonPromptAttachment(kind: imageTypes.contains(mime) ? .image : .file,
                name: item.name, mediaType: mime,
                data: store.attachmentData(payloadID: payload.id, attachmentID: item.id))
        }
        return try HolonPromptRequest(clientRequestID: payload.id.uuidString,
            text: SharedImportStore.sendingText(text: payload.text, urls: payload.urls), attachments: attachments)
    }
    func close() async { await client.close() }
}
