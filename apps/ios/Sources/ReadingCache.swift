import Foundation
import HolonClient

/// Only confirmed reading data belongs here, never credentials or expanded activity.
@MainActor
final class ReadingCache {
    struct Entry: Codable, Sendable {
        let partition: ReadingPartition
        let agentID: String?
        var agents: [ReadingAgent]
        var snapshot: JSONValue?
        var position: String?
        var savedAt: Date
        var briefs: [String: JSONValue]
        /// Oldest first. Missing order falls back to stable, lexicographic IDs.
        var briefOrder: [String]
        var rosterEpoch: String?

        init(partition: ReadingPartition, agentID: String?, agents: [ReadingAgent],
             snapshot: JSONValue?, position: String?, savedAt: Date,
             briefs: [String: JSONValue] = [:], briefOrder: [String] = [], rosterEpoch: String? = nil) {
            self.partition = partition
            self.agentID = agentID
            self.agents = agents
            self.snapshot = snapshot
            self.position = position
            self.savedAt = savedAt
            self.briefs = briefs
            self.briefOrder = briefOrder
            self.rosterEpoch = rosterEpoch
        }

        private enum CodingKeys: String, CodingKey {
            case partition, agentID, agents, snapshot, position, savedAt, briefs, briefOrder, rosterEpoch
        }

        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            partition = try values.decode(ReadingPartition.self, forKey: .partition)
            agentID = try values.decodeIfPresent(String.self, forKey: .agentID)
            agents = try values.decode([ReadingAgent].self, forKey: .agents)
            snapshot = try values.decodeIfPresent(JSONValue.self, forKey: .snapshot)
            position = try values.decodeIfPresent(String.self, forKey: .position)
            savedAt = try values.decode(Date.self, forKey: .savedAt)
            briefs = try values.decodeIfPresent([String: JSONValue].self, forKey: .briefs) ?? [:]
            briefOrder = try values.decodeIfPresent([String].self, forKey: .briefOrder) ?? []
            rosterEpoch = try values.decodeIfPresent(String.self, forKey: .rosterEpoch)
        }
    }

    private struct Key: Hashable {
        let partition: ReadingPartition
        let agentID: String?
        init(_ entry: Entry) { partition = entry.partition; agentID = entry.agentID }
    }

    private let storage: ReadingCacheStorage
    private var entries: [Entry] = []
    private var prepared = false
    private var preparation: Task<Void, Never>?
    private var persistence: Task<Bool, Never>?
    private var revision: UInt64 = 0
    private var savedBeforePreparation: Set<Key> = []
    private var removedBeforePreparation: Set<ReadingPartition> = []
    private var ignoreInitialRead = false

    init() {
        storage = ReadingCacheStorage(defaultFiles: {
            (ReadingCache.defaultFile(), ReadingCache.legacyFile())
        })
        startPreparation()
    }

    /// An explicit URL (including nil for memory-only) never triggers legacy migration.
    init(file: URL?) {
        storage = ReadingCacheStorage(file: file)
        prepared = file == nil
        startPreparation()
    }

    /// Resolve default locations on the storage actor; injectable for isolated migration tests.
    init(defaultFiles: @escaping @Sendable () -> (URL?, URL?)) {
        storage = ReadingCacheStorage(defaultFiles: defaultFiles)
        startPreparation()
    }

    nonisolated static func defaultFile() -> URL? {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent("HolonReading", isDirectory: true)
            .appendingPathComponent("confirmed-reading.json")
    }

    nonisolated private static func legacyFile() -> URL? {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?
            .appendingPathComponent("HolonReading", isDirectory: true)
            .appendingPathComponent("confirmed-reading.json")
    }

    /// Concurrent callers share one disk read. Synchronous mutations win over that read.
    func prepare() async {
        startPreparation()
        await preparation?.value
    }

    private func startPreparation() {
        guard !prepared, preparation == nil else { return }
        let storage = storage
        let read = Task.detached { await storage.read() }
        preparation = Task { [self] in
            let loaded = await read.value
            let untouched = ignoreInitialRead ? [] : loaded.filter {
                !savedBeforePreparation.contains(Key($0)) &&
                !removedBeforePreparation.contains($0.partition)
            }
            entries = Array((untouched + entries).suffix(ReadingCacheLimits.maximumEntries))
            prepared = true
            savedBeforePreparation.removeAll()
            removedBeforePreparation.removeAll()
            preparation = nil
        }
    }

    func load(_ partition: ReadingPartition, agentID: String?) -> Entry? {
        guard let index = entries.lastIndex(where: {
            $0.partition == partition && $0.agentID == agentID &&
            ReadingCacheLimits.isValidTimestamp($0.savedAt)
        }) else { return nil }
        let entry = entries[index]
        if index != entries.count - 1 {
            entries.remove(at: index)
            entries.append(entry)
            revision &+= 1
            schedulePersistence()
        }
        return entry
    }

    /// False rejects an invalid or oversized entry without changing existing records or pending writes.
    /// JSON encoding expansion is checked off-main and may instead fail flush().
    @discardableResult
    func save(_ entry: Entry) -> Bool {
        // Structural memory guards only; all JSON encoding and exact byte checks are off-main.
        guard let bounded = ReadingCacheLimits.boundMemory(entry) else { return false }
        if !prepared, !ignoreInitialRead {
            savedBeforePreparation.insert(Key(bounded))
            boundPreparationFences()
        }
        entries.removeAll {
            ($0.partition == bounded.partition && $0.agentID == bounded.agentID) ||
            !ReadingCacheLimits.isValidTimestamp($0.savedAt)
        }
        entries.append(bounded)
        entries = Array(entries.suffix(ReadingCacheLimits.maximumEntries))
        revision &+= 1
        schedulePersistence()
        return true
    }

    func remove(_ partition: ReadingPartition) {
        if !prepared, !ignoreInitialRead {
            removedBeforePreparation.insert(partition)
            boundPreparationFences()
        }
        entries.removeAll { $0.partition == partition }
        revision &+= 1
        schedulePersistence()
    }

    /// True means the latest queued state was written successfully (or this cache is memory-only).
    /// A failed flush can be retried; it never reports a best-effort write as durable.
    @discardableResult
    func flush() async -> Bool {
        await prepare()
        schedulePersistence()
        return await persistence!.value
    }

    private func boundPreparationFences() {
        // If a caller floods mutations before yielding, prefer a cache miss to unbounded tombstones.
        if savedBeforePreparation.count + removedBeforePreparation.count > ReadingCacheLimits.maximumEntries {
            ignoreInitialRead = true
            savedBeforePreparation.removeAll()
            removedBeforePreparation.removeAll()
        }
    }

    private func schedulePersistence() {
        guard persistence == nil else { return }
        persistence = Task { [self] in
            await prepare()
            while true {
                let writingRevision = revision
                let result = await storage.write(entries)
                // One writer, not a task per save: an old write cannot run after a newer removal.
                guard writingRevision == revision else { continue }
                entries = result.entries
                persistence = nil
                return result.succeeded
            }
        }
    }
}

private enum ReadingCacheLimits {
    static let maximumBodyBytes = 256 * 1_024
    static let maximumBriefBytes = 2 * 1_024 * 1_024
    static let maximumBriefs = 80
    static let maximumEntries = 12
    static let maximumFileBytes = 16 * 1_024 * 1_024

    static func isValidTimestamp(_ date: Date, now: Date = Date()) -> Bool {
        // Age indicates freshness, not a deadline for offline readability.
        date.timeIntervalSince1970.isFinite && date <= now
    }

    private static func textCost(_ text: String, limit: Int) -> Int? {
        let count = text.utf8.prefix(limit + 1).count
        return count <= limit ? count : nil
    }

    /// Conservative retained-memory budget, not a JSON byte measurement.
    private static func bodyCost(_ value: JSONValue, limit: Int) -> Int? {
        var remaining = limit
        func charge(_ cost: Int) -> Bool {
            guard cost <= remaining else { return false }
            remaining -= cost
            return true
        }
        func visit(_ value: JSONValue, depth: Int) -> Bool {
            guard depth < 64, charge(32) else { return false }
            switch value {
            case .string(let string):
                guard let cost = textCost(string, limit: remaining) else { return false }
                return charge(cost)
            case .array(let values):
                guard values.count <= remaining / 32 else { return false }
                for value in values where !visit(value, depth: depth + 1) { return false }
            case .object(let values):
                guard values.count <= remaining / 32 else { return false }
                for (key, value) in values {
                    guard let cost = textCost(key, limit: remaining), charge(cost),
                          visit(value, depth: depth + 1) else { return false }
                }
            case .integer, .number, .bool, .null: break
            }
            return true
        }
        return visit(value, depth: 0) ? limit - remaining : nil
    }

    private static func briefKeys(_ entry: ReadingCache.Entry) -> [String] {
        var ordered: [String] = []
        for key in entry.briefOrder.suffix(maximumBriefs)
            where entry.briefs[key] != nil && textCost(key, limit: 512) != nil {
            if !ordered.contains(key) { ordered.append(key) }
        }
        // A full oversized order already specifies which old bodies must be evicted.
        if entry.briefOrder.count > maximumBriefs { return ordered }
        // Keep the fallback selection itself bounded, even for an oversized incoming dictionary.
        var fallback: [String] = []
        for key in entry.briefs.keys
            where !ordered.contains(key) && textCost(key, limit: 512) != nil {
            fallback.append(key)
            fallback.sort()
            if fallback.count > maximumBriefs { fallback.removeFirst() }
        }
        return Array((ordered + fallback).suffix(maximumBriefs))
    }

    static func boundMemory(_ original: ReadingCache.Entry) -> ReadingCache.Entry? {
        guard isValidTimestamp(original.savedAt),
              let url = URLComponents(string: original.partition.api),
              url.host != nil, url.user == nil, url.password == nil,
              !original.partition.runtime.isEmpty, !original.partition.user.isEmpty,
              !original.partition.visibility.isEmpty else { return nil }
        var metadata = 0
        func charge(_ text: String?) -> Bool {
            guard let text else { return true }
            guard let cost = textCost(text, limit: maximumBodyBytes - metadata) else { return false }
            metadata += cost + 32
            return metadata <= maximumBodyBytes
        }
        let partition = original.partition
        guard [partition.api, partition.network, partition.runtime, partition.user, partition.visibility]
            .allSatisfy({ charge($0) }),
              charge(original.agentID), charge(original.position), charge(original.rosterEpoch),
              original.agents.count <= 512 else { return nil }
        for agent in original.agents {
            guard charge(agent.id), charge(agent.name), charge(agent.preview),
                  charge(agent.operatorPreview), charge(agent.currentRunID), charge(agent.posture),
                  charge(agent.effectiveModel) else { return nil }
        }
        var entry = original
        if let snapshot = entry.snapshot, bodyCost(snapshot, limit: maximumBodyBytes) == nil {
            return nil
        }
        var remaining = maximumBriefBytes
        var briefs: [String: JSONValue] = [:]
        var order: [String] = []
        for key in briefKeys(entry).reversed() {
            guard let body = entry.briefs[key],
                  let cost = bodyCost(body, limit: maximumBodyBytes),
                  cost + key.utf8.count + 32 <= remaining else { continue }
            remaining -= cost + key.utf8.count + 32
            briefs[key] = body
            order.append(key)
        }
        entry.briefs = briefs
        entry.briefOrder = Array(order.reversed())
        return entry
    }

    /// Oldest access first; capacity eviction does not change savedAt freshness.
    /// Only called by the storage actor; encoding may further evict brief bodies.
    static func normalize(_ entries: [ReadingCache.Entry]) -> [ReadingCache.Entry] {
        let encoder = JSONEncoder()
        var result: [ReadingCache.Entry] = []
        for original in entries {
            guard var entry = boundMemory(original) else { continue }
            if let snapshot = entry.snapshot,
               ((try? encoder.encode(snapshot).count) ?? Int.max) > maximumBodyBytes {
                continue
            }
            var remaining = maximumBriefBytes
            for key in entry.briefOrder.reversed() {
                guard let body = entry.briefs[key] else { continue }
                let size = (try? encoder.encode(body).count) ?? Int.max
                let keySize = (try? encoder.encode(key).count) ?? 1_024
                if size > maximumBodyBytes || size > remaining - keySize - 1 {
                    entry.briefs.removeValue(forKey: key)
                } else {
                    remaining -= size + keySize + 1
                }
            }
            entry.briefOrder.removeAll { entry.briefs[$0] == nil }
            result.removeAll { $0.partition == entry.partition && $0.agentID == entry.agentID }
            result.append(entry)
        }
        result = Array(result.suffix(maximumEntries))
        // Individual encodes avoid repeatedly encoding a multi-megabyte file while evicting.
        var sizes = result.map { (try? encoder.encode($0).count) ?? maximumFileBytes }
        var total = sizes.reduce(64, +) // Version envelope and array separators.
        while total > maximumFileBytes, !result.isEmpty {
            total -= sizes.removeFirst()
            result.removeFirst()
        }
        return result
    }
}

/// Synchronous actor methods keep path resolution, Codable work and disk IO on one background lane.
private actor ReadingCacheStorage {
    private struct Document: Codable {
        let version: Int
        let entries: [ReadingCache.Entry]
    }

    struct WriteResult: Sendable {
        let entries: [ReadingCache.Entry]
        let succeeded: Bool
    }

    private var file: URL?
    private var defaultFiles: (@Sendable () -> (URL?, URL?))?
    private let memoryOnly: Bool
    private var legacyToRemove: URL?

    init(file: URL?) { self.file = file; memoryOnly = file == nil }
    init(defaultFiles: @escaping @Sendable () -> (URL?, URL?)) {
        self.defaultFiles = defaultFiles
        memoryOnly = false
    }

    func read() -> [ReadingCache.Entry] {
        if let resolve = defaultFiles {
            let (destination, legacy) = resolve()
            file = destination
            defaultFiles = nil
            if let destination, let legacy,
               !FileManager.default.fileExists(atPath: destination.path),
               let entries = decode(legacy) {
                legacyToRemove = legacy
                return write(entries).entries
            }
        }
        return file.flatMap(decode).map(ReadingCacheLimits.normalize) ?? []
    }

    private func decode(_ file: URL) -> [ReadingCache.Entry]? {
        do {
            let handle = try FileHandle(forReadingFrom: file)
            defer { try? handle.close() }
            let data = try handle.read(upToCount: ReadingCacheLimits.maximumFileBytes + 1) ?? Data()
            guard data.count <= ReadingCacheLimits.maximumFileBytes else { return nil }
            let decoder = JSONDecoder()
            if let document = try? decoder.decode(Document.self, from: data) {
                return document.version == 1 ? document.entries : nil
            }
            // The original cache was an unversioned array with no briefs or body order.
            return try? decoder.decode([ReadingCache.Entry].self, from: data)
        } catch {
            return nil
        }
    }

    func write(_ entries: [ReadingCache.Entry]) -> WriteResult {
        let encoder = JSONEncoder()
        guard entries.allSatisfy({ entry in
            guard let snapshot = entry.snapshot else { return true }
            return ((try? encoder.encode(snapshot).count) ?? Int.max) <= ReadingCacheLimits.maximumBodyBytes
        }) else {
            // Preserve the durable file and report failure so callers can reduce their turns window.
            return .init(entries: entries.compactMap(ReadingCacheLimits.boundMemory), succeeded: false)
        }
        let normalized = ReadingCacheLimits.normalize(entries)
        guard let file else { return .init(entries: normalized, succeeded: memoryOnly) }
        do {
            let data = try JSONEncoder().encode(Document(version: 1, entries: normalized))
            guard data.count <= ReadingCacheLimits.maximumFileBytes else {
                return .init(entries: normalized, succeeded: false)
            }
            let folder = file.deletingLastPathComponent()
            #if os(iOS)
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true,
                                                   attributes: [.protectionKey: FileProtectionType.complete])
            try data.write(to: file, options: [.atomic, .completeFileProtection])
            #else
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try data.write(to: file, options: [.atomic])
            #endif
            var excluded = URLResourceValues()
            excluded.isExcludedFromBackup = true
            var directoryURL = folder
            var fileURL = file
            try directoryURL.setResourceValues(excluded)
            try fileURL.setResourceValues(excluded)
            if let legacy = legacyToRemove {
                try FileManager.default.removeItem(at: legacy)
                legacyToRemove = nil
            }
            return .init(entries: normalized, succeeded: true)
        } catch {
            return .init(entries: normalized, succeeded: false)
        }
    }
}
