import Foundation

public enum HolonAvatarExpression: String, CaseIterable, Hashable, Sendable {
    case neutral
    case focused
    case questioning
    case waiting
    case sleeping
}

extension HolonClient {
    /// Downloads passive PNG bytes; callers negotiate `agent_avatar_v1` through the authenticated handshake.
    public func agentAvatar(agentID: String, expression: HolonAvatarExpression = .neutral, size: Int = 128)
        async throws -> HolonResponse<HolonDownloadedArtifact> {
        guard [32, 64, 128, 256, 512].contains(size) else { throw HolonClientError.invalidRequest }
        return try await downloadBinary(
            path: ["agents", agentID, "avatar"],
            query: ["version": "light-mech-1-vivid-2", "expression": expression.rawValue,
                    "format": "png", "size": String(size)],
            maximumBytes: 2 * 1024 * 1024,
            allowedContentTypes: ["image/png"])
    }
}
