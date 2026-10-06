import CoreData
import XCTest
@testable import Holon

@MainActor
final class PlatformFoundationTests: XCTestCase {
    func testKeychainRoundTripUpdateIsolationAndRemoval() throws {
        let vault = CredentialVault(service: "run.holon.ios.tests.\(UUID().uuidString)")
        let first = "network-a/runtime-a/user-a/private"
        let second = "network-b/runtime-b/user-b/private"
        defer {
            try? vault.remove(account: first)
            try? vault.remove(account: second)
        }
        XCTAssertNil(try vault.read(account: first))
        try vault.write(Data("session-a".utf8), account: first)
        try vault.write(Data("session-b".utf8), account: second)
        try vault.write(Data("session-a-renewed".utf8), account: first)
        XCTAssertEqual(try vault.read(account: first), Data("session-a-renewed".utf8))
        XCTAssertEqual(try vault.read(account: second), Data("session-b".utf8))
        try vault.remove(account: first)
        XCTAssertNil(try vault.read(account: first))
        XCTAssertEqual(try vault.read(account: second), Data("session-b".utf8))
    }

    /// Probe the storage contract before introducing a production outbox schema.
    func testCoreDataTransactionSurvivesStoreReopenAndRollback() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("probe.sqlite")
        let entity = NSEntityDescription()
        entity.name = "PendingInput"
        entity.managedObjectClassName = "NSManagedObject"
        let attributes = ["requestID", "payload", "status"].map { name in
            let attribute = NSAttributeDescription()
            attribute.name = name
            attribute.attributeType = .stringAttributeType
            attribute.isOptional = false
            return attribute
        }
        entity.properties = attributes
        let model = NSManagedObjectModel()
        model.entities = [entity]
        let coordinator = NSPersistentStoreCoordinator(managedObjectModel: model)
        let store = try coordinator.addPersistentStore(ofType: NSSQLiteStoreType, configurationName: nil, at: url)
        let context = NSManagedObjectContext(concurrencyType: .mainQueueConcurrencyType)
        context.persistentStoreCoordinator = coordinator
        let record = NSEntityDescription.insertNewObject(forEntityName: "PendingInput", into: context)
        record.setValuesForKeys(["requestID": "original-request", "payload": "immutable", "status": "unknown"])
        try context.save()
        record.setValue("wrong-new-request", forKey: "requestID")
        context.rollback()
        XCTAssertEqual(record.value(forKey: "requestID") as? String, "original-request")
        context.reset()
        try coordinator.remove(store)
        let reopened = NSPersistentStoreCoordinator(managedObjectModel: model)
        let reopenedStore = try reopened.addPersistentStore(ofType: NSSQLiteStoreType, configurationName: nil, at: url)
        defer { try? reopened.remove(reopenedStore) }
        let recoveredContext = NSManagedObjectContext(concurrencyType: .mainQueueConcurrencyType)
        recoveredContext.persistentStoreCoordinator = reopened
        let records = try recoveredContext.fetch(NSFetchRequest<NSManagedObject>(entityName: "PendingInput"))
        XCTAssertEqual(records.count, 1)
        XCTAssertEqual(records[0].value(forKey: "requestID") as? String, "original-request")
        XCTAssertEqual(records[0].value(forKey: "payload") as? String, "immutable")
        XCTAssertEqual(records[0].value(forKey: "status") as? String, "unknown")
    }

    func testPlatformNetworkPolicyAllowsConfirmedUserSuppliedEndpoints() {
        let policy = Bundle.main.object(forInfoDictionaryKey: "NSAppTransportSecurity") as? [String: Any]
        XCTAssertEqual(policy?["NSAllowsArbitraryLoads"] as? Bool, true)
        // Presence of this key would override arbitrary loads on current iOS.
        XCTAssertNil(policy?["NSAllowsLocalNetworking"])
    }
}
