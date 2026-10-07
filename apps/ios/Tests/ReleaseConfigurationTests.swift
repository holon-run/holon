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

    func testHostedAppAndEmbeddedShareExtensionHaveMatchingVersions() throws {
        let info = try packagedInfoDictionary()
        let version = try XCTUnwrap(info["CFBundleShortVersionString"] as? String)
        let build = try XCTUnwrap(info["CFBundleVersion"] as? String)
        XCTAssertFalse(version.isEmpty)
        XCTAssertGreaterThan(Int(build) ?? 0, 0)

        let plugins = Bundle.main.bundleURL.appendingPathComponent("PlugIns")
        let extensions = try FileManager.default.contentsOfDirectory(
            at: plugins, includingPropertiesForKeys: nil, options: .skipsHiddenFiles
        ).filter { $0.pathExtension == "appex" }
        XCTAssertEqual(extensions.count, 1)
        let share = try XCTUnwrap(extensions.first)
        let shareInfo = try packagedInfoDictionary(in: share)
        XCTAssertEqual(shareInfo["CFBundleShortVersionString"] as? String, version)
        XCTAssertEqual(shareInfo["CFBundleVersion"] as? String, build)
    }

    private func packagedInfoDictionary(in bundleURL: URL = Bundle.main.bundleURL) throws -> [String: Any] {
        // Bundle's runtime lookup resolves device qualifiers; inspect both variants as shipped.
        let url = bundleURL.appendingPathComponent("Info.plist")
        let data = try Data(contentsOf: url)
        return try XCTUnwrap(
            PropertyListSerialization.propertyList(from: data, options: [], format: nil) as? [String: Any]
        )
    }
}
