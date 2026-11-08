import Foundation
import HolonClient

protocol AgentAvatarTransport: Sendable {
    func supportsAvatars() async throws -> Bool
    func image(agentID: String, expression: HolonAvatarExpression) async throws -> Data
}

/// Shares the reader's authenticated SDK client, not another session or image URL.
actor AgentAvatarClientTransport: AgentAvatarTransport {
    private let client: HolonClient
    private let authority: HolonConnectionIdentity
    private var bound: HolonConnectionIdentity?

    init(client: HolonClient, authority: HolonConnectionIdentity) {
        self.client = client
        self.authority = authority
    }

    private func expected() async throws -> HolonConnectionIdentity {
        let current = await client.identity
        guard let runtime = authority.runtimeID, !runtime.isEmpty,
              let user = authority.userID, !user.isEmpty,
              let visibility = authority.visibilityScopeID, !visibility.isEmpty,
              current.networkID == authority.networkID, current.runtimeID == runtime,
              current.userID == user, current.visibilityScopeID == visibility,
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
            throw HolonHTTPFailure(statusCode: failure.statusCode, apiError: failure.apiError, identity: authority)
        }
    }

    func supportsAvatars() async throws -> Bool {
        let handshake = try await request { try await $0.handshake() }
        guard case .compatible = handshake.checkCompatibility() else { return false }
        return handshake.server.capabilities.contains("agent_avatar_v1")
    }

    func image(agentID: String, expression: HolonAvatarExpression) async throws -> Data {
        try await request { try await $0.agentAvatar(agentID: agentID, expression: expression) }.data
    }
}
