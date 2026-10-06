import Foundation
import XCTest
@testable import HolonMenu

final class HolonLocalizationTests: XCTestCase {
    func testDefaultLocaleFollowsSystemLanguagePreferences() {
        let language = Bundle.preferredLocalizations(
            from: ["en", "zh-Hans"], forPreferences: Locale.preferredLanguages
        ).first
        if let expectedLanguage = ProcessInfo.processInfo.environment["HOLON_TEST_EXPECTED_LOCALIZATION"] {
            XCTAssertEqual(language, expectedLanguage)
        }
        XCTAssertEqual(L10n.text("Settings…"), language == "zh-Hans" ? "设置…" : "Settings…")
    }

    func testExplicitLocalesAndEnglishFallback() {
        XCTAssertEqual(L10n.text("Settings…", locale: Locale(identifier: "en")), "Settings…")
        XCTAssertEqual(L10n.text("Settings…", locale: Locale(identifier: "zh-Hans")), "设置…")
        XCTAssertEqual(L10n.text("Settings…", locale: Locale(identifier: "fr")), "Settings…")
        XCTAssertEqual(L10n.text("unknown diagnostic", locale: Locale(identifier: "zh-Hans")), "unknown diagnostic")
    }

    func testActualLifecycleStateTitles() {
        XCTAssertEqual(HolonDaemonLifecycleState.running.title(locale: Locale(identifier: "en")), "Running")
        XCTAssertEqual(HolonDaemonLifecycleState.running.title(locale: Locale(identifier: "zh-Hans")), "运行中")
        XCTAssertEqual(HolonDaemonLifecycleState.versionMismatch.title(locale: Locale(identifier: "zh-Hans")), "版本不匹配")
    }

    func testCatalogParityAndFormatArguments() throws {
        let resources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/HolonMenu/Resources")
        func catalog(_ language: String) throws -> [String: String] {
            let data = try Data(contentsOf: resources.appendingPathComponent("\(language).lproj/Localizable.strings"))
            return try XCTUnwrap(PropertyListSerialization.propertyList(from: data, format: nil) as? [String: String])
        }
        let english = try catalog("en")
        let chinese = try catalog("zh-Hans")
        XCTAssertEqual(Set(english.keys), Set(chinese.keys))
        for (key, value) in english {
            XCTAssertFalse(chinese[key]!.isEmpty, key)
            XCTAssertEqual(value.components(separatedBy: "%@").count,
                           chinese[key]!.components(separatedBy: "%@").count, key)
        }
        let path = "/Users/example/holon"
        XCTAssertEqual(L10n.format("Installed at %@. Add ~/.local/bin to PATH if needed.", path,
                                   locale: Locale(identifier: "zh-Hans")),
                       "已安装到 /Users/example/holon。如有需要，请将 ~/.local/bin 添加到 PATH。")
    }
}
