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
    }

    func testMissingURLFallsBackToGenericDisclosure() {
        XCTAssertFalse(PrivacyDisclosure.text(for: nil).isEmpty)
        let invalid = URL(string: "/relative")!
        XCTAssertFalse(PrivacyDisclosure.text(for: invalid).isEmpty)
    }
}
