import XCTest
@testable import Holon

final class ReleaseConfigurationTests: XCTestCase {
    func testHostedAppHasCompiledPrimaryIconsForPhoneAndPad() throws {
        let info = try packagedInfoDictionary()
        for key in ["CFBundleIcons", "CFBundleIcons~ipad"] {
            let icons = try XCTUnwrap(info[key] as? [String: Any])
            let primary = try XCTUnwrap(icons["CFBundlePrimaryIcon"] as? [String: Any])
            XCTAssertEqual(primary["CFBundleIconName"] as? String, "AppIcon")
            let files = try XCTUnwrap(primary["CFBundleIconFiles"] as? [String])
            XCTAssertFalse(files.isEmpty)
        }
    }

    func testHostedAppPreservesPhoneOrientationsAndSupportsPadMultitasking() throws {
        let info = try packagedInfoDictionary()
        let phoneOrientations = [
            "UIInterfaceOrientationPortrait",
            "UIInterfaceOrientationLandscapeLeft",
            "UIInterfaceOrientationLandscapeRight"
        ]
        XCTAssertEqual(
            info["UISupportedInterfaceOrientations"] as? [String],
            phoneOrientations
        )
        let padOrientations = try XCTUnwrap(
            info["UISupportedInterfaceOrientations~ipad"] as? [String]
        )
        XCTAssertEqual(Set(padOrientations), Set(phoneOrientations + ["UIInterfaceOrientationPortraitUpsideDown"]))
        XCTAssertNotEqual(info["UIRequiresFullScreen"] as? Bool, true)
    }

    private func packagedInfoDictionary() throws -> [String: Any] {
        // Bundle's runtime lookup resolves device qualifiers; inspect both variants as shipped.
        let url = Bundle.main.bundleURL.appendingPathComponent("Info.plist")
        let data = try Data(contentsOf: url)
        return try XCTUnwrap(
            PropertyListSerialization.propertyList(from: data, options: [], format: nil) as? [String: Any]
        )
    }
}
