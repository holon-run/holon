// Generated from docs/website/reference/openapi.json. Do not edit.
import Foundation

public enum WorkspaceProjectionMetadata: Codable, JSONEncodable {
    case managedWorktreeProjectionMetadata(ManagedWorktreeProjectionMetadata)
    case existingGitWorktreeProjectionMetadata(ExistingGitWorktreeProjectionMetadata)

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(ManagedWorktreeProjectionMetadata.self) {
            self = .managedWorktreeProjectionMetadata(value)
            return
        }
        if let value = try? container.decode(ExistingGitWorktreeProjectionMetadata.self) {
            self = .existingGitWorktreeProjectionMetadata(value)
            return
        }
        throw DecodingError.dataCorruptedError(in: container, debugDescription: "No matching anyOf branch for WorkspaceProjectionMetadata")
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .managedWorktreeProjectionMetadata(let value): try container.encode(value)
        case .existingGitWorktreeProjectionMetadata(let value): try container.encode(value)
        }
    }
}
