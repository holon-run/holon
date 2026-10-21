import Foundation
import XCTest
@testable import Holon

final class PrivacyDisclosureTests: XCTestCase {
    func testOfficialDemoRuntimeIsRecognizedAndNamesProvider() {
        let url = URL(string: "https://demo.holon.run/api")!
        XCTAssertTrue(PrivacyDisclosure.isOfficialDemo(url))
        let text = PrivacyDisclosure.text(for: url)
        XCTAssertTrue(text.contains("DeepSeek"), "Demo disclosure must name the AI provider")
        XCTAssertTrue(text.contains("demo.holon.run"), "Demo disclosure must name the runtime host")
    }

    func testHostVariantsOfOfficialDemoAreNotSpecialCased() {
        for raw in ["https://demo.holon.run.evil.test/api",
                    "https://not-demo.holon.run/api"] {
            let url = URL(string: raw)!
            XCTAssertFalse(PrivacyDisclosure.isOfficialDemo(url), raw)
        }
    }

    func testThirdPartyHostIsNamedInline() {
        let url = URL(string: "https://runtime.example.com/api")!
        XCTAssertFalse(PrivacyDisclosure.isOfficialDemo(url))
        let text = PrivacyDisclosure.text(for: url)
        XCTAssertTrue(text.contains("runtime.example.com"),
                      "Third-party disclosure must name the connected host")
        XCTAssertTrue(text.contains("administrator") || text.contains("管理员"),
                      "Third-party disclosure must keep the generic administrator wording")
    }

    func testMissingURLFallsBackToGenericDisclosure() {
        let fallback = PrivacyDisclosure.text(for: nil)
        XCTAssertTrue(fallback.contains("administrator") || fallback.contains("管理员"),
                      "Fallback must use the generic runtime-administrator disclosure")
        let invalid = URL(string: "/relative")!
        XCTAssertFalse(PrivacyDisclosure.text(for: invalid).isEmpty)
    }
}
