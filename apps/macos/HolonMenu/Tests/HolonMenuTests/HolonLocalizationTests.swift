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

    func testLocalizedBundlePreservesResourceDirectorySpelling() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        for (index, language) in ["zh-Hans", "zh-hans"].enumerated() {
            let bundleURL = directory.appendingPathComponent("\(index).bundle", isDirectory: true)
            for (name, value) in [("en", "Settings…"), (language, "设置…")] {
                let localization = bundleURL.appendingPathComponent("\(name).lproj", isDirectory: true)
                try FileManager.default.createDirectory(at: localization, withIntermediateDirectories: true)
                try "\"Settings…\" = \"\(value)\";".write(
                    to: localization.appendingPathComponent("Localizable.strings"),
                    atomically: true, encoding: .utf8
                )
            }
            let resources = try XCTUnwrap(Bundle(url: bundleURL))
            let chinese = L10n.localizedBundle(for: Locale(identifier: "zh-Hans"), in: resources)
            XCTAssertEqual(chinese.bundleURL.lastPathComponent, "\(language).lproj")
            XCTAssertEqual(chinese.localizedString(forKey: "Settings…", value: nil, table: nil), "设置…")
            let fallback = L10n.localizedBundle(for: Locale(identifier: "fr"), in: resources)
            XCTAssertEqual(fallback.bundleURL.lastPathComponent, "en.lproj")
            XCTAssertEqual(fallback.localizedString(forKey: "Settings…", value: nil, table: nil), "Settings…")
        }
    }

    func testActualLifecycleStateTitles() {
        XCTAssertEqual(HolonDaemonLifecycleState.running.title(locale: Locale(identifier: "en")), "Running")
        XCTAssertEqual(HolonDaemonLifecycleState.running.title(locale: Locale(identifier: "zh-Hans")), "运行中")
        XCTAssertEqual(HolonDaemonLifecycleState.versionMismatch.title(locale: Locale(identifier: "zh-Hans")), "版本不匹配")
    }

    func testHealthyRuntimeActivityMessages() {
        let messages = [
            ("runtime is healthy and idle", "运行时运行正常，当前空闲"),
            ("runtime is healthy and waiting", "运行时运行正常，正在等待"),
            ("runtime is healthy and processing work", "运行时运行正常，正在处理任务")
        ]
        for (english, chinese) in messages {
            XCTAssertEqual(L10n.text(english, locale: Locale(identifier: "en")), english)
            XCTAssertEqual(L10n.text(english, locale: Locale(identifier: "zh-Hans")), chinese)
            XCTAssertEqual(L10n.text(english, locale: Locale(identifier: "fr")), english)
        }
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
