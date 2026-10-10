import Foundation
import XCTest
@testable import Holon

final class SharingConsentTests: XCTestCase {
    func testUnavailableStorageAlwaysDenies() {
        let consent = SharingConsent(defaults: nil)
        let url = URL(string: "https://example.test/api")!
        consent.approve(url)
        XCTAssertThrowsError(try consent.require(url))
    }

    func testApprovalIsVersionedScopedPersistentAndRevocable() throws {
        let suite = "SharingConsentTests.\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let consent = SharingConsent(defaults: defaults)
        let url = URL(string: "https://example.test/api")!
        XCTAssertThrowsError(try consent.require(url))
        consent.approve(url)
        try SharingConsent(defaults: UserDefaults(suiteName: suite)).require(url)
        XCTAssertThrowsError(try consent.require(URL(string: "https://other.test/api")!))
        XCTAssertThrowsError(try consent.require(URL(string: "https://example.test/other/api")!))
        consent.revoke(url)
        XCTAssertThrowsError(try consent.require(url))
    }
}
