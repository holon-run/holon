import XCTest
@testable import Holon

final class PrivacyManifestTests: XCTestCase {
    func testHostedAppDeclaresPrivateAndAppGroupPreferencesUse() throws {
        let url = try XCTUnwrap(Bundle.main.url(forResource: "PrivacyInfo", withExtension: "xcprivacy"))
        let manifest = try XCTUnwrap(
            PropertyListSerialization.propertyList(from: Data(contentsOf: url), format: nil) as? [String: Any]
        )
        let accessedAPIs = try XCTUnwrap(manifest["NSPrivacyAccessedAPITypes"] as? [[String: Any]])
        XCTAssertEqual(accessedAPIs.count, 1)
        let defaults = try XCTUnwrap(accessedAPIs.first)
        XCTAssertEqual(defaults["NSPrivacyAccessedAPIType"] as? String, "NSPrivacyAccessedAPICategoryUserDefaults")
        XCTAssertEqual(defaults["NSPrivacyAccessedAPITypeReasons"] as? [String], ["CA92.1", "1C8F.1"])
    }
}
