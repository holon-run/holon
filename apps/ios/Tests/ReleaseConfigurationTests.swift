import XCTest
@testable import Holon

final class ReleaseConfigurationTests: XCTestCase {
    func testCameraPurposeAndPolishVocabularyAreLocalized() throws {
        XCTAssertTrue((try packagedInfoDictionary()["NSCameraUsageDescription"] as? String)?.contains("Agent") == true)
        for language in ["en", "zh-Hans"] {
            let path = try XCTUnwrap(Bundle.main.path(forResource: language, ofType: "lproj"))
            let bundle = try XCTUnwrap(Bundle(path: path))
            for key in ["agents.filter.all", "agents.filter.reply", "agents.filter.newResults", "agents.filter.active",
                        "sending.camera", "sending.camera.permission", "sending.camera.unavailable", "files.endOfFile"] {
                let value = bundle.localizedString(forKey: key, value: nil, table: nil)
                XCTAssertNotEqual(value, key, "\(language) must resolve \(key)")
                XCTAssertFalse(value.isEmpty)
            }
            let purpose = bundle.localizedString(forKey: "NSCameraUsageDescription", value: nil, table: "InfoPlist")
            XCTAssertNotEqual(purpose, "NSCameraUsageDescription")
            XCTAssertTrue(purpose.contains("Agent"))
        }
    }

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

    func testEmbeddedShareExtensionDeclaresAppGroupPreferencesUse() throws {
        let plugins = Bundle.main.bundleURL.appendingPathComponent("PlugIns")
        let extensions = try FileManager.default.contentsOfDirectory(
            at: plugins, includingPropertiesForKeys: nil, options: .skipsHiddenFiles
        ).filter { $0.pathExtension == "appex" }
        XCTAssertEqual(extensions.count, 1)
        let share = try XCTUnwrap(extensions.first)
        let url = share.appendingPathComponent("PrivacyInfo.xcprivacy")
        let manifest = try XCTUnwrap(
            PropertyListSerialization.propertyList(from: Data(contentsOf: url), format: nil) as? [String: Any]
        )
        let accessedAPIs = try XCTUnwrap(manifest["NSPrivacyAccessedAPITypes"] as? [[String: Any]])
        XCTAssertEqual(accessedAPIs.count, 1)
        let defaults = try XCTUnwrap(accessedAPIs.first)
        XCTAssertEqual(defaults["NSPrivacyAccessedAPIType"] as? String, "NSPrivacyAccessedAPICategoryUserDefaults")
        XCTAssertEqual(defaults["NSPrivacyAccessedAPITypeReasons"] as? [String], ["1C8F.1"])
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
