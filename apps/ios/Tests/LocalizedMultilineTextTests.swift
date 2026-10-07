import SwiftUI
import UIKit
import XCTest
@testable import Holon

@MainActor
final class LocalizedMultilineTextTests: XCTestCase {
    func testMaximumChineseWelcomeUsesFullMultilineSemanticFonts() throws {
        try checkMaximumWelcome(locale: "zh-Hans")
    }

    func testMaximumEnglishWelcomeUsesFullMultilineSemanticFonts() throws {
        try checkMaximumWelcome(locale: "en")
    }

    func testMaximumDiagnosticsUsesFullLocalizedMultilineBody() throws {
        for (locale, expected) in [
            ("zh-Hans", "仅包含连接状态与数量，不包含凭据、身份、地址、消息内容或原始错误。"),
            ("en", "Only connection states and counts are included. Credentials, identities, addresses, message content and raw errors are excluded.")
        ] {
            let (window, host) = try hostText(AnyView(
                LocalizedMultilineText(key: "diagnostics.allowlist")
                    .frame(width: 370, alignment: .leading)
                    .environment(\.locale, Locale(identifier: locale))
                    .dynamicTypeSize(.accessibility5)
            ))
            defer { window.isHidden = true; window.rootViewController = nil }
            let text = try label("diagnostics.allowlist", in: host.view)
            let traits = UITraitCollection(
                preferredContentSizeCategory: .accessibilityExtraExtraExtraLarge)
            XCTAssertEqual(text.text, expected)
            XCTAssertEqual(text.font.pointSize,
                           UIFont.preferredFont(forTextStyle: .body, compatibleWith: traits).pointSize)
            XCTAssertEqual(text.textColor, .secondaryLabel)
            XCTAssertEqual(text.accessibilityTraits, .staticText)
            XCTAssertEqual(text.numberOfLines, 0)
            XCTAssertTrue(text.adjustsFontForContentSizeCategory)
            XCTAssertGreaterThan(text.bounds.width, 0)
            XCTAssertGreaterThan(text.bounds.height, text.font.lineHeight)
            let fitting = text.sizeThatFits(CGSize(width: text.bounds.width,
                                                  height: .greatestFiniteMagnitude))
            XCTAssertEqual(text.bounds.height, fitting.height, accuracy: 0.5)
        }
    }

    func testExistingLabelsUpdateWhenEnvironmentTextSizeChanges() async throws {
        let (window, host) = try hostWelcome(locale: "zh-Hans", size: .large)
        defer { window.isHidden = true; window.rootViewController = nil }
        let title = try label("onboarding.welcome", in: host.view)
        let body = try label("onboarding.purpose", in: host.view)
        let initialHeight = body.bounds.height
        let initialPointSize = body.font.pointSize
        XCTAssertGreaterThan(initialHeight, 0)
        host.rootView = welcome(locale: "zh-Hans", size: .accessibility5)
        try await waitForLayout(host) {
            body.font.pointSize > initialPointSize && body.bounds.height > initialHeight
        }
        XCTAssertTrue(title === (try label("onboarding.welcome", in: host.view)))
        XCTAssertTrue(body === (try label("onboarding.purpose", in: host.view)))
        XCTAssertGreaterThan(body.font.pointSize, initialPointSize)
        XCTAssertGreaterThan(body.bounds.height, initialHeight)
        try checkFontsAndSizing(title: title, body: body)
        host.rootView = welcome(locale: "zh-Hans", size: .large)
        try await waitForLayout(host) {
            body.font.pointSize == initialPointSize &&
                abs(body.bounds.height - initialHeight) < 0.5
        }
        XCTAssertEqual(body.font.pointSize, initialPointSize)
        XCTAssertEqual(body.bounds.height, initialHeight, accuracy: 0.5)
    }

    func testExistingLabelsUpdateWhenEnvironmentLanguageChanges() async throws {
        let (window, host) = try hostWelcome(locale: "zh-Hans", size: .accessibility5)
        defer { window.isHidden = true; window.rootViewController = nil }
        let title = try label("onboarding.welcome", in: host.view)
        let body = try label("onboarding.purpose", in: host.view)
        XCTAssertEqual(title.text, "连接你的 Holon，继续工作")
        host.rootView = welcome(locale: "en", size: .accessibility5)
        try await waitForLayout(host) {
            title.text == "Your Holon, wherever you work" &&
                body.text == "Connect to a running Holon on your computer to view your Agents and work."
        }
        XCTAssertTrue(title === (try label("onboarding.welcome", in: host.view)))
        XCTAssertTrue(body === (try label("onboarding.purpose", in: host.view)))
        try checkFontsAndSizing(title: title, body: body)
        host.rootView = welcome(locale: "zh-Hans", size: .accessibility5)
        try await waitForLayout(host) {
            title.text == "连接你的 Holon，继续工作" &&
                body.text == "连接电脑上正在运行的 Holon，即可查看 Agent 和工作。"
        }
        try checkFontsAndSizing(title: title, body: body)
    }

    private func checkMaximumWelcome(locale: String) throws {
        let (window, host) = try hostWelcome(locale: locale, size: .accessibility5)
        defer { window.isHidden = true; window.rootViewController = nil }
        let title = try label("onboarding.welcome", in: host.view)
        let body = try label("onboarding.purpose", in: host.view)
        XCTAssertFalse(try XCTUnwrap(title.text).isEmpty)
        XCTAssertFalse(try XCTUnwrap(body.text).isEmpty)
        if locale == "zh-Hans" {
            XCTAssertEqual(title.text, "连接你的 Holon，继续工作")
            XCTAssertEqual(body.text, "连接电脑上正在运行的 Holon，即可查看 Agent 和工作。")
        } else {
            XCTAssertEqual(title.text, "Your Holon, wherever you work")
            XCTAssertEqual(body.text,
                           "Connect to a running Holon on your computer to view your Agents and work.")
        }
        try checkFontsAndSizing(title: title, body: body)
    }

    private func checkFontsAndSizing(title: UILabel, body: UILabel) throws {
        let traits = UITraitCollection(preferredContentSizeCategory: .accessibilityExtraExtraExtraLarge)
        XCTAssertEqual(title.font.pointSize,
                       UIFont.preferredFont(forTextStyle: .title2, compatibleWith: traits).pointSize)
        XCTAssertTrue(title.font.fontDescriptor.symbolicTraits.contains(.traitBold))
        XCTAssertEqual(body.font.pointSize,
                       UIFont.preferredFont(forTextStyle: .body, compatibleWith: traits).pointSize)
        XCTAssertEqual(title.textColor, .label)
        XCTAssertEqual(body.textColor, .secondaryLabel)
        XCTAssertTrue(title.accessibilityTraits.contains(.header))
        XCTAssertFalse(body.accessibilityTraits.contains(.header))
        for text in [title, body] {
            XCTAssertEqual(text.numberOfLines, 0)
            XCTAssertTrue(text.adjustsFontForContentSizeCategory)
            XCTAssertGreaterThan(text.bounds.width, 0)
            let fitting = text.sizeThatFits(CGSize(width: text.bounds.width,
                                                  height: .greatestFiniteMagnitude))
            XCTAssertGreaterThan(text.bounds.height, text.font.lineHeight)
            XCTAssertEqual(text.bounds.height, fitting.height, accuracy: 0.5)
        }
    }

    private func welcome(locale: String, size: DynamicTypeSize) -> AnyView {
        AnyView(VStack(alignment: .leading, spacing: 12) {
            LocalizedMultilineText(key: "onboarding.welcome", isHeading: true)
            LocalizedMultilineText(key: "onboarding.purpose")
        }
        .frame(width: 370, alignment: .leading)
        .environment(\.locale, Locale(identifier: locale))
        .dynamicTypeSize(size))
    }

    private func waitForLayout(_ host: UIHostingController<AnyView>,
                               until condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(5)
        repeat {
            host.view.layoutIfNeeded()
            if condition(),
               let title = try? label("onboarding.welcome", in: host.view),
               let body = try? label("onboarding.purpose", in: host.view),
               [title, body].allSatisfy({ text in
                   let fitting = text.sizeThatFits(CGSize(width: text.bounds.width,
                                                         height: .greatestFiniteMagnitude))
                   return text.bounds.width > 0 && abs(text.bounds.height - fitting.height) < 0.5
               }) { return }
            try await Task.sleep(for: .milliseconds(10))
        } while ContinuousClock.now < deadline
        XCTFail("SwiftUI did not finish updating hosted welcome text")
    }

    private func hostWelcome(locale: String, size: DynamicTypeSize)
        throws -> (UIWindow, UIHostingController<AnyView>) {
        try hostText(welcome(locale: locale, size: size))
    }

    private func hostText(_ view: AnyView) throws -> (UIWindow, UIHostingController<AnyView>) {
        let host = UIHostingController(rootView: view)
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }.first,
            "Hosted welcome tests need a real UIWindowScene")
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 1000)
        window.rootViewController = host
        window.makeKeyAndVisible()
        host.view.layoutIfNeeded()
        return (window, host)
    }

    private func label(_ key: String, in view: UIView) throws -> UILabel {
        func find(in view: UIView) -> UILabel? {
            if let label = view as? UILabel, label.accessibilityIdentifier == key { return label }
            for child in view.subviews {
                if let label = find(in: child) { return label }
            }
            return nil
        }
        return try XCTUnwrap(find(in: view))
    }
}
