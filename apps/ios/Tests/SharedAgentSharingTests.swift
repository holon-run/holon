import Foundation
import HolonClient
import UniformTypeIdentifiers
import UIKit
import XCTest
@testable import Holon

@MainActor
final class SharedAgentSharingTests: XCTestCase {
    func testDirectShareRequiresConsentBeforeNetworkOrDeliveryMutation() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = try SharedImportStore(container: directory)
        let payload = try store.stage(text: "local only", urls: [], files: [])
        let vault = SharedSessionVault(accessGroup: "unavailable.test.group", service: UUID().uuidString)
        let sender = try SharedAgentSender(session: session(), vault: vault,
            consent: SharingConsent(defaults: nil))
        do {
            try await sender.send(payloadID: payload.id, agentID: "A", store: store)
            XCTFail("Sending without consent must fail")
        } catch {
            XCTAssertTrue(error is SharingConsentRequired)
        }
        XCTAssertNil(try store.load().first?.delivery)
        XCTAssertEqual(try store.load().first?.text, "local only")
        await sender.close()
    }

    private func session(generation: UUID = UUID(), user: String = "user") -> SharedSession {
        SharedSession(generation: generation, networkID: "network", connectionName: "Fixture",
            apiBaseURL: URL(string: "http://127.0.0.1:7878/api/")!, allowInsecureHTTP: true,
            runtimeID: "runtime", userID: user, visibilityScopeID: "visibility",
            credential: "isolated-secret", expiresAt: Date().addingTimeInterval(300))
    }
    func testGroupKeychainIsNarrowRevocableAndGenerationChecked() throws {
        let group = try SharedSessionVault.configured().accessGroup
        let vault = SharedSessionVault(accessGroup: group, service: "test.share." + UUID().uuidString)
        defer { try? vault.clear() }
        let first = session()
        try vault.write(first)
        XCTAssertEqual(try vault.read(), first)
        XCTAssertFalse(first.description.contains(first.credential))
        XCTAssertNoThrow(try vault.require(first))
        try vault.write(session(user: "other"))
        XCTAssertThrowsError(try vault.require(first))
        try vault.clear()
        XCTAssertNil(try vault.read())
        XCTAssertThrowsError(try vault.require(first))
    }
    func testDeliveryPersistsSameUUIDAndCannotChangeTargetOrAcceptedState() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = try SharedImportStore(container: directory)
        let original = try store.stage(text: "original", urls: [URL(string: "https://example.test")!],
            files: [.init(name: "report.txt", typeIdentifier: UTType.plainText.identifier, data: Data("bytes".utf8))])
        let target = SharedShareTarget(session: session(), agentID: "holon-tester")
        let prepared = try store.prepareDelivery(id: original.id, target: target)
        let request = try SharedAgentSender.prompt(prepared, store: store)
        try store.markDelivery(id: original.id, target: target, state: .unknown)
        let reopened = try SharedImportStore(container: directory)
        let recovered = try XCTUnwrap(reopened.load().first)
        XCTAssertEqual(recovered.delivery?.state, .unknown)
        XCTAssertEqual(try SharedAgentSender.prompt(recovered, store: reopened), request)
        XCTAssertEqual(request.clientRequestID, original.id.uuidString)
        XCTAssertThrowsError(try reopened.prepareDelivery(id: original.id,
            target: SharedShareTarget(session: session(user: "other"), agentID: "holon-tester")))
        XCTAssertThrowsError(try reopened.prepareDelivery(id: original.id,
            target: SharedShareTarget(session: session(), agentID: "other")))
        try reopened.markDelivery(id: original.id, target: target, state: .accepted)
        XCTAssertThrowsError(try reopened.markDelivery(id: original.id, target: target, state: .unknown))
        XCTAssertThrowsError(try reopened.prepareDelivery(id: original.id, target: target))
        let json = try JSONEncoder().encode(reopened.load())
        XCTAssertFalse(String(decoding: json, as: UTF8.self).contains("isolated-secret"))
    }
    func testTextAndWebURLProvidersStayTextAndURL() async throws {
        let text = NSItemProvider(object: "line one\nline two" as NSString)
        guard case .text(let value) = try await SharedProviderLoader.load(text) else { return XCTFail("Text provider") }
        XCTAssertEqual(value, "line one\nline two")
        let url = NSItemProvider(object: URL(string: "https://example.test/report.txt")! as NSURL)
        url.suggestedName = "report.txt"
        guard case .url(let value) = try await SharedProviderLoader.load(url) else { return XCTFail("Web URL is not a file") }
        XCTAssertEqual(value.host, "example.test")
    }
    func testDataOnlyNamedTextAndImageProvidersAreAttachments() async throws {
        for type in [UTType.plainText, .png] {
            let provider = NSItemProvider()
            provider.suggestedName = type == .png ? "image" : "note.txt"
            provider.registerDataRepresentation(forTypeIdentifier: type.identifier, visibility: .all) { callback in
                callback(Data("provider bytes".utf8), nil); return nil
            }
            guard case .file(let file) = try await SharedProviderLoader.load(provider) else { return XCTFail("Named/data provider is an attachment") }
            XCTAssertEqual(file.data, Data("provider bytes".utf8))
            XCTAssertEqual(file.typeIdentifier, type.identifier)
            XCTAssertEqual(file.name, type == .png ? "image.png" : "note.txt")
        }
    }
    func testUIKitImageObjectIsEncodedAsPNGNotAnObjectArchive() async throws {
        let format = UIGraphicsImageRendererFormat(); format.scale = 1
        let image = UIGraphicsImageRenderer(size: CGSize(width: 16, height: 16), format: format).image {
            UIColor.systemBlue.setFill(); $0.fill(CGRect(x: 0, y: 0, width: 16, height: 16))
        }
        for provider in [NSItemProvider(object: image), NSItemProvider(item: image, typeIdentifier: UTType.image.identifier)] {
            provider.suggestedName = "image"
            guard case .file(let file) = try await SharedProviderLoader.load(provider) else { return XCTFail("Image attachment") }
            XCTAssertEqual(file.typeIdentifier, UTType.png.identifier)
            XCTAssertEqual(file.name, "image.png")
            XCTAssertTrue(file.data.starts(with: [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))
        }
    }
    func testHostHidesBoundImportsUntilMatchingIdentityIsActivated() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = try SharedImportStore(container: directory)
        let payload = try store.stage(text: "bound", urls: [], files: [])
        let session = session()
        _ = try store.prepareDelivery(id: payload.id, target: .init(session: session, agentID: "holon-tester"))
        let coordinator = SharedImportCoordinator(store: store)
        coordinator.reload()
        XCTAssertTrue(coordinator.pending.isEmpty)
        let identity = HolonConnectionIdentity(networkID: session.networkID, runtimeID: session.runtimeID,
            userID: session.userID, visibilityScopeID: session.visibilityScopeID)
        coordinator.activate(try XCTUnwrap(ReadingPartition(apiBaseURL: session.apiBaseURL, identity: identity)))
        XCTAssertEqual(coordinator.pending.map(\.id), [payload.id])
        coordinator.resetDestination()
        XCTAssertTrue(coordinator.pending.isEmpty)
    }
}
