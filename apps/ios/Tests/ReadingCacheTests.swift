import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class ReadingCacheTests: XCTestCase {
    func testBoundedCapacityAndPartitionKeys() throws {
        let identity = HolonConnectionIdentity(networkID: "wifi", runtimeID: "runtime",
                                               userID: "user", visibilityScopeID: "private")
        let partition = try XCTUnwrap(ReadingPartition(
            apiBaseURL: URL(string: "https://cache.example/api")!, identity: identity))
        let cache = ReadingCache(file: nil)
        for index in 0..<20 {
            cache.save(.init(partition: partition, agentID: String(index), agents: [],
                             snapshot: nil, position: nil, savedAt: Date()))
        }
        XCTAssertNil(cache.load(partition, agentID: "0"))
        XCTAssertNotNil(cache.load(partition, agentID: "19"))
        cache.save(.init(partition: partition, agentID: "oversized", agents: [],
                         snapshot: .string(String(repeating: "x", count: 300_000)),
                         position: nil, savedAt: Date()))
        XCTAssertNil(cache.load(partition, agentID: "oversized"))
        cache.save(.init(partition: partition, agentID: "expired", agents: [],
                         snapshot: nil, position: nil, savedAt: Date(timeIntervalSinceNow: -8 * 24 * 60 * 60)))
        XCTAssertNil(cache.load(partition, agentID: "expired"))
        XCTAssertNil(ReadingPartition(apiBaseURL: URL(string: "https://user:secret@cache.example/api")!,
                                      identity: identity))
        cache.remove(partition)
        XCTAssertNil(cache.load(partition, agentID: "19"))
    }

    func testCorruptCacheDegradesWithoutThrowing() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("cache.json")
        try Data("not json".utf8).write(to: file)
        let cache = ReadingCache(file: file)
        let identity = HolonConnectionIdentity(networkID: "wifi", runtimeID: "runtime",
                                               userID: "user", visibilityScopeID: "private")
        let partition = try XCTUnwrap(ReadingPartition(
            apiBaseURL: URL(string: "https://cache.example/api")!, identity: identity))
        XCTAssertNil(cache.load(partition, agentID: nil))
        cache.save(.init(partition: partition, agentID: nil,
                         agents: [.init(id: "A", name: "Agent", preview: "recent")],
                         snapshot: nil, position: nil, savedAt: Date()))
        XCTAssertEqual(ReadingCache(file: file).load(partition, agentID: nil)?.agents.first?.id, "A")
    }
}
