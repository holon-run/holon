import XCTest
@testable import Holon

final class ConversationImageTests: XCTestCase {
    func testMarkdownFindsOnlyAuthorizedServerReferenceForms() {
        let markdown = """
            ![screenshot](/srv/agent/media/generated/example%20one.png)
            ![workspace](workspace://example/media/two.png?root=selected)
            ![external](https://example.invalid/hidden.png)
            ![relative](images/unscoped.png)
            ![authority](file://example.invalid/tmp/remote.png)
            ![repeat](/srv/agent/media/generated/example%20one.png)
            `![literal](/srv/agent/media/generated/ignored.png)`
            """
        XCTAssertEqual(ConversationImageReference.markdown(markdown), [
            .init(reference: "/srv/agent/media/generated/example one.png", label: "screenshot"),
            .init(reference: "workspace://example/media/two.png?root=selected", label: "workspace"),
        ])
    }

    func testOperatorAttachmentsUseCanonicalUploadedImageLines() {
        let text = String(repeating: "long caption ", count: 3_000) + """


            ![screenshot.png](/srv/agent/media/inbox/request%2Dtest%2D1%2Dpreview%2Epng)
            [document.pdf](/srv/agent/media/inbox/request%2Dtest%2D2%2Ddocument%2Epdf)
            ![other](/srv/agent/media/generated/outside.png)
            ![remote](https://example.invalid/media/inbox/remote.png)
            """
        XCTAssertEqual(ConversationImageReference.operatorAttachments(text), [
            .init(reference: "/srv/agent/media/inbox/request-test-1-preview.png", label: "screenshot.png"),
        ])
    }

    func testThumbnailRejectsInvalidDataAndNonImageTypes() throws {
        let png = try XCTUnwrap(Data(base64Encoded:
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNQKDgAAAIEAVHqHS4xAAAAAElFTkSuQmCC"))
        let thumbnail = try XCTUnwrap(ConversationImageThumbnail.decode(png, mediaType: "image/png"))
        XCTAssertEqual(thumbnail.width, 1)
        XCTAssertEqual(thumbnail.height, 1)
        XCTAssertNil(ConversationImageThumbnail.decode(png, mediaType: "text/plain"))
        XCTAssertNil(ConversationImageThumbnail.decode(Data("not an image".utf8), mediaType: "image/png"))
    }
}
