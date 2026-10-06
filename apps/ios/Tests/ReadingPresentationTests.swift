import HolonClient
import XCTest
@testable import Holon

@MainActor
final class ReadingPresentationTests: XCTestCase {
    func testTextEnvelopeDisplaysContentWithoutTranslatingIt() {
        let input: JSONValue = .object([
            "preview": .string(#"{"type":"text","text":"Please inspect the report\n保持原文"}"#)
        ])
        XCTAssertEqual(ReadingPresentation.operatorText(input), "Please inspect the report\n保持原文")
    }

    func testPlainTruncatedAndFuturePreviewRemainVerbatim() {
        for preview in [
            "plain <script>content</script>",
            #"{"type":"text","text":"truncated"#,
            #"{"type":"future","text":"Do not reinterpret"}"#
        ] {
            XCTAssertEqual(ReadingPresentation.operatorText(.object(["preview": .string(preview)])), preview)
        }
    }
}
