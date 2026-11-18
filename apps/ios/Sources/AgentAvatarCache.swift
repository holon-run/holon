import Foundation
import HolonClient

protocol AgentAvatarCaching: Sendable {
    func load(_ partition: ReadingPartition) async -> AgentAvatarCache.Snapshot?
    func save(_ snapshot: AgentAvatarCache.Snapshot) async
    func remove(_ partition: ReadingPartition) async
}

/// Decorative PNGs only. One bounded, identity-partitioned cache; never credentials.
actor AgentAvatarCache: AgentAvatarCaching {
    struct Image: Codable, Sendable {
        let agentID: String
        let expression: HolonAvatarExpression
        let png: Data

        private enum CodingKeys: String, CodingKey { case agentID, expression, png }
        init(agentID: String, expression: HolonAvatarExpression, png: Data) {
            self.agentID = agentID
            self.expression = expression
            self.png = png
        }
        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            agentID = try values.decode(String.self, forKey: .agentID)
            let raw = try values.decode(String.self, forKey: .expression)
            guard let expression = HolonAvatarExpression(rawValue: raw) else {
                throw DecodingError.dataCorruptedError(forKey: .expression, in: values, debugDescription: "Unknown expression")
            }
            self.expression = expression
            png = try values.decode(Data.self, forKey: .png)
        }
        func encode(to encoder: Encoder) throws {
            var values = encoder.container(keyedBy: CodingKeys.self)
            try values.encode(agentID, forKey: .agentID)
            try values.encode(expression.rawValue, forKey: .expression)
            try values.encode(png, forKey: .png)
        }
    }
    struct Snapshot: Codable, Sendable {
        // Bump with the SDK's avatar artwork version, size or decoding contract.
        var artwork = "light-mech-1-vivid-2/128/png"
        let partition: ReadingPartition
        let epoch: String
        let images: [Image]
    }

    private let file: URL?
    private static let maximumFileBytes = 12 * 1_024 * 1_024

    init(file: URL? = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?
        .appendingPathComponent("Holon/avatars-v1.json")) {
        self.file = file
    }

    func load(_ partition: ReadingPartition) -> Snapshot? {
        guard let file, let handle = try? FileHandle(forReadingFrom: file) else { return nil }
        defer { try? handle.close() }
        guard let data = try? handle.read(upToCount: Self.maximumFileBytes + 1),
              data.count <= Self.maximumFileBytes,
              let snapshot = try? JSONDecoder().decode(Snapshot.self, from: data),
              snapshot.artwork == "light-mech-1-vivid-2/128/png",
              snapshot.partition == partition, snapshot.images.count <= 128 else { return nil }
        return snapshot
    }

    func save(_ snapshot: Snapshot) {
        guard let file, snapshot.images.count <= 128,
              let data = try? JSONEncoder().encode(snapshot),
              data.count <= Self.maximumFileBytes else { return }
        do {
            try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            try data.write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch { /* An evictable decorative cache must not interrupt reading. */ }
    }

    func remove(_ partition: ReadingPartition) {
        guard load(partition) != nil, let file else { return }
        try? FileManager.default.removeItem(at: file)
    }
}
