import Foundation
import HolonClient

/// One bounded file; authentication material and expanded detail never enter it.
@MainActor
final class ReadingCache {
    struct Entry: Codable {
        let partition: ReadingPartition
        let agentID: String?
        var agents: [ReadingAgent]
        var snapshot: JSONValue?
        var position: String?
        var savedAt: Date
    }

    private let file: URL?
    private var entries: [Entry] = []
    private let maximumEntryBytes = 262_144
    private let maximumFileBytes = 4_194_304
    private let maximumEntries = 12
    private let lifetime: TimeInterval = 7 * 24 * 60 * 60
    private var writeOptions: Data.WritingOptions {
        #if os(iOS)
        return [.atomic, .completeFileProtection]
        #else
        return [.atomic]
        #endif
    }

    init(file: URL? = ReadingCache.defaultFile()) {
        self.file = file
        if let file,
           let size = try? file.resourceValues(forKeys: [.fileSizeKey]).fileSize,
           size <= maximumFileBytes,
           let data = try? Data(contentsOf: file),
           let decoded = try? JSONDecoder().decode([Entry].self, from: data) {
            entries = Array(decoded.suffix(maximumEntries)).filter {
                Date().timeIntervalSince($0.savedAt) < lifetime &&
                $0.savedAt <= Date() &&
                ((try? JSONEncoder().encode($0).count) ?? Int.max) <= maximumEntryBytes
            }
        }
    }

    nonisolated static func defaultFile() -> URL? {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?
            .appendingPathComponent("HolonReading", isDirectory: true)
            .appendingPathComponent("confirmed-reading.json")
    }

    func load(_ partition: ReadingPartition, agentID: String?) -> Entry? {
        entries.last {
            $0.partition == partition && $0.agentID == agentID &&
            Date().timeIntervalSince($0.savedAt) < lifetime
        }
    }

    func save(_ entry: Entry) {
        guard let encoded = try? JSONEncoder().encode(entry),
              encoded.count <= maximumEntryBytes else { return }
        entries.removeAll { $0.partition == entry.partition && $0.agentID == entry.agentID }
        entries.append(entry)
        entries = Array(entries.suffix(maximumEntries))
        guard let file, let data = try? JSONEncoder().encode(entries),
              data.count <= maximumFileBytes else { return }
        do {
            try FileManager.default.createDirectory(at: file.deletingLastPathComponent(),
                                                    withIntermediateDirectories: true)
            try data.write(to: file, options: writeOptions)
        } catch {
            // Read-only offline support degrades to this bounded in-memory cache.
        }
    }

    func remove(_ partition: ReadingPartition) {
        entries.removeAll { $0.partition == partition }
        guard let file, let data = try? JSONEncoder().encode(entries) else { return }
        try? data.write(to: file, options: writeOptions)
    }
}
