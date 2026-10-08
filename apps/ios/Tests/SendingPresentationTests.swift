import XCTest
@testable import Holon

final class SendingPresentationTests: XCTestCase {
    func testStatusKeysAreResolvedStringsNotLocalizedInterpolationPatterns() {
        XCTAssertEqual(SendingPresentation.statusKey(.ready), "sending.status.ready")
        XCTAssertEqual(SendingPresentation.statusKey(.offline), "sending.status.offline")
        XCTAssertEqual(SendingPresentation.statusKey(.sending), "sending.status.sending")
        XCTAssertEqual(SendingPresentation.statusKey(.disconnected), "sending.status.disconnected")
    }
    func testReceiptNeverMeansAgentCompleted() {
        XCTAssertEqual(SendingPresentation.stateKey(.received), "sending.state.received")
        XCTAssertFalse(SendingPresentation.canRetry(.received))
        XCTAssertFalse(SendingPresentation.canRetry(.sending))
        XCTAssertTrue(SendingPresentation.canRetry(.unknown))
        XCTAssertTrue(SendingPresentation.canRetry(.failed))
        XCTAssertTrue(SendingPresentation.canRetry(.queued))
    }

    func testDraftRequiresTextOrAttachment() {
        XCTAssertFalse(SendingPresentation.hasContent(SendingDraft(text: " \n ")))
        XCTAssertTrue(SendingPresentation.hasContent(SendingDraft(text: "hello")))
        XCTAssertTrue(SendingPresentation.hasContent(SendingDraft(attachments: [
            SendingAttachment(id: UUID(), name: "photo", byteCount: 1, contentType: "image/jpeg")
        ])))
    }
}
