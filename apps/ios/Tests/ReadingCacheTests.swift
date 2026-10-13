import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class ReadingCacheTests: XCTestCase {
    private func partition(server: String = "cache.example", network: String = "wifi",
                           runtime: String = "runtime", user: String = "user",
                           scope: String = "private") throws -> ReadingPartition {
        try XCTUnwrap(ReadingPartition(
            apiBaseURL: URL(string: "https://\(server)/api")!,
            identity: HolonConnectionIdentity(networkID: network, runtimeID: runtime,
                                             userID: user, visibilityScopeID: scope)))
    }

    private func folder() throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    private func entry(_ partition: ReadingPartition, agentID: String? = "A",
                       text: String = "confirmed", savedAt: Date = Date(),
                       briefs: [String: JSONValue] = [:],
                       briefOrder: [String] = []) -> ReadingCache.Entry {
        .init(partition: partition, agentID: agentID,
              agents: [.init(id: "A", name: "Agent", preview: "recent")],
              snapshot: .object(["text": .string(text)]), position: "cursor", savedAt: savedAt,
              briefs: briefs, briefOrder: briefOrder)
    }

    private func assertFlushed(_ cache: ReadingCache,
                               file: StaticString = #filePath, line: UInt = #line) async {
        let succeeded = await cache.flush()
        XCTAssertTrue(succeeded, file: file, line: line)
    }

    func testBoundedCapacityAndPartitionKeys() async throws {
        let partition = try partition()
        let cache = ReadingCache(file: nil)
        // Memory-only load/save/remove remain synchronous, without preparing first.
        for index in 0..<20 { cache.save(entry(partition, agentID: String(index))) }
        // Twelve entries: capacity, not age, makes the first eight unreadable.
        XCTAssertNil(cache.load(partition, agentID: "7"))
        XCTAssertNotNil(cache.load(partition, agentID: "8"))
        XCTAssertNotNil(cache.load(partition, agentID: "19"))
        XCTAssertTrue(cache.save(entry(partition, agentID: "old",
                                      savedAt: Date(timeIntervalSinceNow: -8 * 24 * 60 * 60))))
        XCTAssertFalse(cache.save(entry(partition, agentID: "future", savedAt: Date(timeIntervalSinceNow: 60))))
        XCTAssertNotNil(cache.load(partition, agentID: "old"))
        XCTAssertNil(cache.load(partition, agentID: "future"))
        let identity = HolonConnectionIdentity(networkID: "wifi", runtimeID: "runtime",
                                               userID: "user", visibilityScopeID: "private")
        XCTAssertNil(ReadingPartition(apiBaseURL: URL(string: "https://user:secret@cache.example/api")!,
                                      identity: identity))
        cache.remove(partition)
        XCTAssertNil(cache.load(partition, agentID: "old"))
        await assertFlushed(cache)
        XCTAssertNil(cache.load(partition, agentID: "19"))
    }

    func testOversizedSnapshotRejectsSaveWithoutReplacingConfirmedData() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let cache = ReadingCache(file: file)
        XCTAssertTrue(cache.save(entry(partition)))
        await assertFlushed(cache)
        XCTAssertFalse(cache.save(entry(partition, text: String(repeating: "x", count: 300_000))))
        XCTAssertEqual(cache.load(partition, agentID: "A")?.snapshot?["text"], .string("confirmed"))
        await assertFlushed(cache)
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        XCTAssertEqual(reopened.load(partition, agentID: "A")?.snapshot?["text"], .string("confirmed"))
    }

    func testRejectedSavePreservesConfirmedDataBeforeAndAfterPrepare() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let confirmed = entry(partition, savedAt: Date(timeIntervalSinceNow: -30 * 24 * 60 * 60),
                              briefs: ["body": .string("confirmed body")], briefOrder: ["body"])
        let initial = ReadingCache(file: file)
        XCTAssertTrue(initial.save(confirmed))
        await assertFlushed(initial)
        let durable = try Data(contentsOf: file)
        let memory = ReadingCache(file: nil)
        XCTAssertTrue(memory.save(confirmed))
        let reopened = ReadingCache(file: file)
        let rejected = [
            entry(partition, text: "future", savedAt: Date(timeIntervalSinceNow: 60)),
            entry(partition, text: "nan", savedAt: Date(timeIntervalSince1970: .nan)),
            entry(partition, text: "infinity", savedAt: Date(timeIntervalSince1970: .infinity)),
            entry(partition, text: "negative infinity", savedAt: Date(timeIntervalSince1970: -.infinity)),
            entry(partition, text: String(repeating: "x", count: 300_000))
        ]
        // Rejection before prepare must not fence the existing disk record out of the shared read.
        for candidate in rejected {
            XCTAssertFalse(memory.save(candidate))
            XCTAssertFalse(reopened.save(candidate))
        }
        XCTAssertEqual(try Data(contentsOf: file), durable)
        await reopened.prepare()
        for cache in [memory, reopened] {
            for candidate in rejected {
                XCTAssertFalse(cache.save(candidate))
            }
            let preserved = try XCTUnwrap(cache.load(partition, agentID: "A"))
            XCTAssertEqual(preserved.agents, confirmed.agents)
            XCTAssertEqual(preserved.snapshot, confirmed.snapshot)
            XCTAssertEqual(preserved.position, confirmed.position)
            XCTAssertEqual(preserved.savedAt, confirmed.savedAt)
            XCTAssertEqual(preserved.briefs, confirmed.briefs)
            XCTAssertEqual(preserved.briefOrder, confirmed.briefOrder)
            await assertFlushed(cache)
        }
        let restarted = ReadingCache(file: file)
        await restarted.prepare()
        let preserved = try XCTUnwrap(restarted.load(partition, agentID: "A"))
        XCTAssertEqual(preserved.snapshot, confirmed.snapshot)
        XCTAssertEqual(preserved.briefs, confirmed.briefs)
        XCTAssertEqual(preserved.savedAt, confirmed.savedAt)
    }

    func testLRUCapacitySurvivesRestartWithoutChangingFreshness() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let cache = ReadingCache(file: file)
        await cache.prepare()
        let old = Date(timeIntervalSinceNow: -365 * 24 * 60 * 60)
        for index in 0..<12 {
            cache.save(entry(partition, agentID: String(index), savedAt: old))
        }
        XCTAssertEqual(cache.load(partition, agentID: "0")?.savedAt, old)
        cache.save(entry(partition, agentID: "12"))
        XCTAssertNil(cache.load(partition, agentID: "1"))
        await assertFlushed(cache)
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        reopened.save(entry(partition, agentID: "13"))
        XCTAssertNil(reopened.load(partition, agentID: "2"))
        XCTAssertEqual(reopened.load(partition, agentID: "0")?.savedAt, old)
        XCTAssertNotNil(reopened.load(partition, agentID: "13"))
        await assertFlushed(reopened)
    }

    func testDurableBodiesAndEveryIdentityBoundary() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partitions = try [
            partition(), partition(server: "another.example"), partition(network: "another-network"),
            partition(runtime: "another-runtime"), partition(user: "another-user"),
            partition(scope: "another-scope")
        ]
        let cache = ReadingCache(file: file)
        await cache.prepare()
        for (index, partition) in partitions.enumerated() {
            cache.save(entry(partition, text: String(index),
                             briefs: ["body": .object(["markdown": .string("正文 \(index)")])]))
        }
        cache.save(entry(partitions[0], agentID: "B", text: "another-agent",
                         briefs: ["body": .string("B-only")]))
        await assertFlushed(cache)
        let reopened = ReadingCache(file: file)
        XCTAssertNil(reopened.load(partitions[0], agentID: "A"))
        await reopened.prepare()
        for (index, partition) in partitions.enumerated() {
            let loaded = try XCTUnwrap(reopened.load(partition, agentID: "A"))
            XCTAssertEqual(loaded.snapshot?["text"], .string(String(index)))
            XCTAssertEqual(loaded.briefs["body"]?["markdown"], .string("正文 \(index)"))
            XCTAssertEqual(loaded.position, "cursor")
        }
        XCTAssertEqual(reopened.load(partitions[0], agentID: "B")?.briefs["body"], .string("B-only"))
        XCTAssertNil(reopened.load(partitions[0], agentID: "unknown"))
        XCTAssertTrue(try file.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup == true)
        #if os(iOS) && !targetEnvironment(simulator)
        // Simulator does not expose device Data Protection attributes.
        let protection = try file.resourceValues(forKeys: [.fileProtectionKey]).fileProtection
        XCTAssertEqual(protection, .complete)
        #endif
    }

    func testLongOfflineSnapshotAndBriefBodiesSurviveSaveAndRestart() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let savedAt = Date(timeIntervalSince1970:
            Date().timeIntervalSince1970.rounded(.down) - 30 * 24 * 60 * 60)
        let cache = ReadingCache(file: file)
        await cache.prepare()
        XCTAssertTrue(cache.save(entry(partition, text: "offline snapshot", savedAt: savedAt,
                                      briefs: ["body": .object(["markdown": .string("offline body")])])))
        XCTAssertEqual(cache.load(partition, agentID: "A")?.savedAt, savedAt)
        await assertFlushed(cache)

        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        let restored = try XCTUnwrap(reopened.load(partition, agentID: "A"))
        XCTAssertEqual(restored.snapshot?["text"], .string("offline snapshot"))
        XCTAssertEqual(restored.briefs["body"]?["markdown"], .string("offline body"))
        XCTAssertEqual(restored.position, "cursor")
        XCTAssertEqual(restored.savedAt, savedAt)
        XCTAssertTrue(reopened.save(entry(partition, agentID: "B")))
        await assertFlushed(reopened)

        let reopenedAgain = ReadingCache(file: file)
        await reopenedAgain.prepare()
        let retained = try XCTUnwrap(reopenedAgain.load(partition, agentID: "A"))
        XCTAssertEqual(retained.snapshot, restored.snapshot)
        XCTAssertEqual(retained.briefs, restored.briefs)
        XCTAssertEqual(retained.agents, restored.agents)
        XCTAssertEqual(retained.position, restored.position)
        XCTAssertEqual(retained.savedAt, savedAt)
    }

    func testOldJSONDefaultsAndDefaultLocationMigration() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let legacy = folder.appendingPathComponent("Caches/confirmed-reading.json")
        let destination = folder.appendingPathComponent("ApplicationSupport/confirmed-reading.json")
        let partition = try partition()
        let original = entry(partition, savedAt: Date(timeIntervalSinceNow: -30 * 24 * 60 * 60))
        try writeLegacy([original], to: legacy)
        let cache = ReadingCache(defaultFiles: { (destination, legacy) })
        await cache.prepare()
        let loaded = try XCTUnwrap(cache.load(partition, agentID: "A"))
        XCTAssertEqual(loaded.briefs, [:])
        XCTAssertEqual(loaded.briefOrder, [])
        XCTAssertEqual(loaded.agents.first?.id, "A")
        XCTAssertEqual(loaded.savedAt, original.savedAt)
        XCTAssertTrue(FileManager.default.fileExists(atPath: destination.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: legacy.path))
        await assertFlushed(cache)
        let reopened = ReadingCache(file: destination)
        await reopened.prepare()
        XCTAssertEqual(reopened.load(partition, agentID: "A")?.snapshot, original.snapshot)
        XCTAssertEqual(reopened.load(partition, agentID: "A")?.savedAt, original.savedAt)
        XCTAssertTrue(ReadingCache.defaultFile()?.path.contains("Application Support") == true)
    }

    func testNewEpochReplacesOldBodiesBeforePrepareAndAfterRestart() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let cache = ReadingCache(file: file)
        await cache.prepare()
        var old = entry(partition, briefs: ["reused": .string("old body")])
        old.snapshot = .object(["event_log_epoch": .string("old")])
        cache.save(old)
        await assertFlushed(cache)

        let reopened = ReadingCache(file: file)
        var fresh = entry(partition)
        fresh.snapshot = .object(["event_log_epoch": .string("new")])
        // A save before prepare must also fence the old epoch's disk entry.
        reopened.save(fresh)
        await reopened.prepare()
        XCTAssertEqual(reopened.load(partition, agentID: "A")?.briefs, [:])
        await assertFlushed(reopened)

        let latest = ReadingCache(file: file)
        await latest.prepare()
        XCTAssertEqual(latest.load(partition, agentID: "A")?.snapshot?["event_log_epoch"], .string("new"))
        XCTAssertEqual(latest.load(partition, agentID: "A")?.briefs, [:])
        fresh.briefs = ["reused": .string("new body")]
        latest.save(fresh)
        await assertFlushed(latest)
        let final = ReadingCache(file: file)
        await final.prepare()
        XCTAssertEqual(final.load(partition, agentID: "A")?.briefs["reused"], .string("new body"))
    }

    func testMigrationFailurePreservesLegacyUntilSuccessfulRetry() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let legacy = folder.appendingPathComponent("legacy.json")
        let blocked = folder.appendingPathComponent("blocked")
        let destination = blocked.appendingPathComponent("cache.json")
        let partition = try partition()
        try writeLegacy([entry(partition)], to: legacy)
        try Data("not a directory".utf8).write(to: blocked)
        let cache = ReadingCache(defaultFiles: { (destination, legacy) })
        await cache.prepare()
        XCTAssertNotNil(cache.load(partition, agentID: "A"))
        let failed = await cache.flush()
        XCTAssertFalse(failed)
        XCTAssertTrue(FileManager.default.fileExists(atPath: legacy.path))
        try FileManager.default.removeItem(at: blocked)
        await assertFlushed(cache)
        XCTAssertFalse(FileManager.default.fileExists(atPath: legacy.path))
        let reopened = ReadingCache(file: destination)
        await reopened.prepare()
        XCTAssertNotNil(reopened.load(partition, agentID: "A"))
    }

    func testExplicitPathDoesNotMigrateOrReadAnotherFile() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let legacy = folder.appendingPathComponent("legacy.json")
        let partition = try partition()
        try writeLegacy([entry(partition)], to: legacy)
        let cache = ReadingCache(file: folder.appendingPathComponent("custom.json"))
        await cache.prepare()
        XCTAssertNil(cache.load(partition, agentID: "A"))
        await assertFlushed(cache)
        XCTAssertTrue(FileManager.default.fileExists(atPath: legacy.path))
    }

    func testCorruptFutureAndUnknownVersionDegradeSafelyWithoutExpiringOldData() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        for corrupt in ["not json", "{\"version\":99,\"entries\":[]}"] {
            try Data(corrupt.utf8).write(to: file)
            let cache = ReadingCache(file: file)
            await cache.prepare()
            XCTAssertNil(cache.load(partition, agentID: "A"))
            cache.save(entry(partition))
            await assertFlushed(cache)
            let reopened = ReadingCache(file: file)
            await reopened.prepare()
            XCTAssertNotNil(reopened.load(partition, agentID: "A"))
        }
        struct Document: Encodable {
            let version = 1
            let entries: [ReadingCache.Entry]
        }
        try JSONEncoder().encode(Document(entries: [
            entry(partition, agentID: "old", text: "old snapshot",
                  savedAt: Date(timeIntervalSinceNow: -8 * 24 * 60 * 60),
                  briefs: ["body": .string("old body")]),
            entry(partition, agentID: "future", savedAt: Date(timeIntervalSinceNow: 60)),
            entry(partition, agentID: "current")
        ])).write(to: file)
        let cache = ReadingCache(file: file)
        await cache.prepare()
        let old = try XCTUnwrap(cache.load(partition, agentID: "old"))
        XCTAssertEqual(old.snapshot?["text"], .string("old snapshot"))
        XCTAssertEqual(old.briefs["body"], .string("old body"))
        XCTAssertNil(cache.load(partition, agentID: "future"))
        XCTAssertNotNil(cache.load(partition, agentID: "current"))
        try Data(repeating: 32, count: 16 * 1_024 * 1_024 + 1).write(to: file)
        let oversizedFile = ReadingCache(file: file)
        await oversizedFile.prepare()
        XCTAssertNil(oversizedFile.load(partition, agentID: "current"))
    }

    func testBriefCountBudgetAndStableEviction() async throws {
        let partition = try partition()
        let cache = ReadingCache(file: nil)
        let small = Dictionary(uniqueKeysWithValues: (0..<100).map {
            (String(format: "%03d", $0), JSONValue.string("brief \($0)"))
        })
        cache.save(entry(partition, briefs: small))
        let bounded = try XCTUnwrap(cache.load(partition, agentID: "A"))
        XCTAssertEqual(bounded.briefs.count, 80)
        XCTAssertNil(bounded.briefs["019"])
        XCTAssertEqual(bounded.briefOrder.first, "020")
        await assertFlushed(cache)
        XCTAssertEqual(cache.load(partition, agentID: "A")?.briefOrder, bounded.briefOrder)

        let newestFirstIDs = (0..<100).map { String(format: "%03d", $0) }
        cache.save(entry(partition, briefs: small, briefOrder: Array(newestFirstIDs.reversed())))
        XCTAssertNil(cache.load(partition, agentID: "A")?.briefs["080"])
        XCTAssertNotNil(cache.load(partition, agentID: "A")?.briefs["000"])
        XCTAssertEqual(cache.load(partition, agentID: "A")?.briefOrder.first, "079")
        await assertFlushed(cache)

        var large = Dictionary(uniqueKeysWithValues: (0..<80).map {
            (String(format: "%03d", $0), JSONValue.string(String(repeating: "x", count: 40_000)))
        })
        large["oversized"] = .string(String(repeating: "x", count: 300_000))
        cache.save(entry(partition, briefs: large))
        let memoryBounded = try XCTUnwrap(cache.load(partition, agentID: "A"))
        XCTAssertLessThanOrEqual(memoryBounded.briefs.count, 52)
        XCTAssertNil(memoryBounded.briefs["oversized"])
        XCTAssertNotNil(memoryBounded.snapshot)
        await assertFlushed(cache)
        let bodies = try XCTUnwrap(cache.load(partition, agentID: "A")).briefs
        XCTAssertLessThanOrEqual(try JSONEncoder().encode(bodies).count, 2 * 1_024 * 1_024)
    }

    func testEncodingExpansionDropsOnlyBriefBodyAndRespectsOrder() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let cache = ReadingCache(file: file)
        // Cheap memory bounds accept this; JSON escaping exceeds 256 KiB off-main.
        let escaped = String(repeating: "\u{01}", count: 50_000)
        cache.save(entry(partition,
                         briefs: ["old": .string(escaped), "new": .string("keep")],
                         briefOrder: ["old", "new"]))
        await assertFlushed(cache)
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        let loaded = try XCTUnwrap(reopened.load(partition, agentID: "A"))
        XCTAssertEqual(loaded.snapshot?["text"], .string("confirmed"))
        XCTAssertNil(loaded.briefs["old"])
        XCTAssertEqual(loaded.briefs["new"], .string("keep"))
        XCTAssertEqual(loaded.agents.first?.id, "A")
        XCTAssertEqual(loaded.position, "cursor")
    }

    func testSnapshotEncodingExpansionFailsFlushWithoutOverwritingDurableData() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let cache = ReadingCache(file: file)
        cache.save(entry(partition))
        await assertFlushed(cache)
        let escaped = String(repeating: "\u{01}", count: 50_000)
        XCTAssertTrue(cache.save(entry(partition, text: escaped)))
        let failed = await cache.flush()
        XCTAssertFalse(failed)
        XCTAssertEqual(cache.load(partition, agentID: "A")?.snapshot?["text"], .string(escaped))
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        XCTAssertEqual(reopened.load(partition, agentID: "A")?.snapshot?["text"], .string("confirmed"))
        cache.save(entry(partition, text: "reduced-window"))
        await assertFlushed(cache)
        let retried = ReadingCache(file: file)
        await retried.prepare()
        XCTAssertEqual(retried.load(partition, agentID: "A")?.snapshot?["text"], .string("reduced-window"))
    }

    func testWholeFileLimitEvictsOldestEntries() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let cache = ReadingCache(file: file)
        await cache.prepare()
        let briefs = Dictionary(uniqueKeysWithValues: (0..<10).map {
            ("body-\($0)", JSONValue.string(String(repeating: "x", count: 200_000)))
        })
        for index in 0..<12 {
            cache.save(entry(partition, agentID: String(index), briefs: briefs))
        }
        await assertFlushed(cache)
        let size = try XCTUnwrap(file.resourceValues(forKeys: [.fileSizeKey]).fileSize)
        XCTAssertLessThanOrEqual(size, 16 * 1_024 * 1_024)
        XCTAssertNil(cache.load(partition, agentID: "0"))
        XCTAssertNotNil(cache.load(partition, agentID: "11"))
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        XCTAssertNil(reopened.load(partition, agentID: "0"))
        XCTAssertEqual(reopened.load(partition, agentID: "11")?.briefs.count, 10)
    }

    func testRemoveBeforePrepareCannotRestoreDiskEntries() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        try writeLegacy([entry(partition)], to: file)
        let cache = ReadingCache(file: file)
        cache.remove(partition)
        await cache.prepare()
        XCTAssertNil(cache.load(partition, agentID: "A"))
        await assertFlushed(cache)
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        XCTAssertNil(reopened.load(partition, agentID: "A"))
    }

    func testInitializationStartsBackgroundReadWithoutPrepare() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        try writeLegacy([entry(partition)], to: file)
        let gate = PreparationGate(file: file)
        let cache = ReadingCache(defaultFiles: { gate.resolve() })
        let started = await Task.detached {
            gate.waitForStart()
        }.value
        XCTAssertTrue(started, "Initialization should start disk loading without an explicit prepare call.")
        gate.resume.signal()
        await cache.prepare()
        XCTAssertEqual(gate.resolveCount, 1)
        XCTAssertNotNil(cache.load(partition, agentID: "A"))
    }

    func testSharedPreparePreservesConcurrentSaveAndRemove() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let removed = try self.partition(user: "removed-user")
        try writeLegacy([entry(partition, text: "disk-old"), entry(removed)], to: file)
        let gate = PreparationGate(file: file)
        let cache = ReadingCache(defaultFiles: { gate.resolve() })
        let first = Task { await cache.prepare() }
        let started = await Task.detached { gate.waitForStart() }.value
        XCTAssertTrue(started)
        let second = Task { await cache.prepare() }
        cache.save(entry(partition, text: "memory-new", briefs: ["body": .string("new")]))
        cache.remove(removed)
        gate.resume.signal()
        await first.value
        await second.value
        XCTAssertEqual(gate.resolveCount, 1)
        XCTAssertEqual(cache.load(partition, agentID: "A")?.snapshot?["text"], .string("memory-new"))
        XCTAssertNil(cache.load(removed, agentID: "A"))
        await assertFlushed(cache)
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        XCTAssertEqual(reopened.load(partition, agentID: "A")?.briefs["body"], .string("new"))
        XCTAssertNil(reopened.load(removed, agentID: "A"))
    }

    func testRapidWritesAndRemovalHaveNoLateResurrection() async throws {
        let folder = try folder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        let partition = try partition()
        let cache = ReadingCache(file: file)
        await cache.prepare()
        for index in 0..<100 {
            cache.save(entry(partition, text: String(index), briefs: ["body": .string(String(index))]))
            if index.isMultiple(of: 5) { await Task.yield() }
            if index.isMultiple(of: 3) { cache.remove(partition) }
        }
        cache.remove(partition)
        await assertFlushed(cache)
        let reopened = ReadingCache(file: file)
        await reopened.prepare()
        XCTAssertNil(reopened.load(partition, agentID: "A"))
        cache.save(entry(partition, text: "after-removal"))
        await assertFlushed(cache)
        let latest = ReadingCache(file: file)
        await latest.prepare()
        XCTAssertEqual(latest.load(partition, agentID: "A")?.snapshot?["text"], .string("after-removal"))
    }

    private func writeLegacy(_ entries: [ReadingCache.Entry], to file: URL) throws {
        struct LegacyEntry: Encodable {
            let partition: ReadingPartition
            let agentID: String?
            let agents: [ReadingAgent]
            let snapshot: JSONValue?
            let position: String?
            let savedAt: Date
        }
        let legacy = entries.map {
            LegacyEntry(partition: $0.partition, agentID: $0.agentID, agents: $0.agents,
                        snapshot: $0.snapshot, position: $0.position, savedAt: $0.savedAt)
        }
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        try JSONEncoder().encode(legacy).write(to: file)
    }
}

/// Pause default path resolution, before the disk read, without blocking MainActor.
private final class PreparationGate: @unchecked Sendable {
    let started = DispatchSemaphore(value: 0)
    let resume = DispatchSemaphore(value: 0)
    private let file: URL
    private let lock = NSLock()
    private var count = 0

    init(file: URL) { self.file = file }
    var resolveCount: Int { lock.withLock { count } }

    func waitForStart() -> Bool {
        started.wait(timeout: .now() + 5) == .success
    }

    func resolve() -> (URL?, URL?) {
        lock.withLock { count += 1 }
        started.signal()
        resume.wait()
        return (file, nil)
    }
}
