import ImageIO
import HolonClient
import UniformTypeIdentifiers
import XCTest
@testable import Holon

final class ConversationImageTests: XCTestCase {
    @MainActor
    func testFourthImageWaitsUntilASlotIsReleased() async throws {
        let slots = ConversationImageSlots()
        for _ in 0..<3 { try await slots.acquire() }
        var admitted = false
        let fourth = await queued(on: slots) { admitted = true }
        XCTAssertFalse(admitted)
        slots.release()
        try await fourth.value
        XCTAssertTrue(admitted)
        slots.release()
        slots.release()
    }

    @MainActor
    func testImageWaitListIsBoundedAndClearedWhenScopeChanges() async throws {
        let slots = ConversationImageSlots(active: 1, waiting: 1)
        try await slots.acquire()
        let old = await queued(on: slots) { XCTFail("Previous scope waiter was admitted") }
        do {
            try await slots.acquire()
            XCTFail("Accepted a request beyond the waiting limit")
        } catch is ConversationPreviewDeferred {}

        slots.cancelWaiting()
        do {
            try await old.value
            XCTFail("Expected previous scope cancellation")
        } catch is CancellationError {}
        var loaded = false
        let current = await queued(on: slots) { loaded = true }
        XCTAssertFalse(loaded)
        slots.release()
        try await current.value
        XCTAssertTrue(loaded)
    }

    @MainActor
    func testInlineMessageChecksAreCoalescedAndBoundedAcrossScopes() async throws {
        let details = ConversationImageDetails()
        let (started, signal) = AsyncStream<String>.makeStream()
        var requests = started.makeAsyncIterator()
        var held: [String: CheckedContinuation<OperatorInputPresentation.Message, Never>] = [:]
        var queuedRequests = 0
        let oldMessage = OperatorInputPresentation.Message(text: "old", uploadedImages: [])
        let newMessage = OperatorInputPresentation.Message(text: "new", uploadedImages: [])

        func request(_ id: String, operation: @escaping @MainActor () async throws -> OperatorInputPresentation.Message)
            async -> Task<OperatorInputPresentation.Message, any Error> {
            let (entered, ready) = AsyncStream<Void>.makeStream()
            let task = Task { @MainActor in
                ready.yield(())
                ready.finish()
                return try await details.load(id, operation: operation)
            }
            for await _ in entered { break }
            return task
        }

        let first = await request("first") {
            await withCheckedContinuation { held["first"] = $0; signal.yield("first") }
        }
        let firstAdmission = await requests.next()
        XCTAssertEqual(firstAdmission, "first")
        let duplicate = await request("first") {
            XCTFail("Started a duplicate request for the same message")
            return oldMessage
        }
        held.removeValue(forKey: "first")?.resume(returning: oldMessage)
        let firstResult = try await first.value
        let duplicateResult = try await duplicate.value
        XCTAssertEqual(firstResult.text, "old")
        XCTAssertEqual(duplicateResult.text, "old")

        var previous: [Task<OperatorInputPresentation.Message, any Error>] = []
        for index in 0..<10 {
            previous.append(await request("\(index)") {
                if index < 2 {
                    return await withCheckedContinuation { held["\(index)"] = $0; signal.yield("\(index)") }
                }
                queuedRequests += 1
                return oldMessage
            })
            if index < 2 {
                let admitted = await requests.next()
                XCTAssertEqual(admitted, "\(index)")
            }
        }
        do {
            _ = try await details.load("overflow") { XCTFail("Exceeded waiting limit"); return oldMessage }
            XCTFail("Expected overflow to defer")
        } catch is ConversationPreviewDeferred {}

        details.cancelAll()
        let current = await request("0") { newMessage }
        for continuation in held.values { continuation.resume(returning: oldMessage) }
        held.removeAll()
        for task in previous {
            do {
                _ = try await task.value
                XCTFail("Previous scope request settled successfully")
            } catch is CancellationError {}
        }
        let currentResult = try await current.value
        XCTAssertEqual(currentResult.text, "new")
        XCTAssertEqual(queuedRequests, 0)
        signal.finish()
    }

    @MainActor
    func testCancelledImageWaiterDoesNotUseTheNextSlot() async throws {
        let slots = ConversationImageSlots()
        for _ in 0..<3 { try await slots.acquire() }
        let cancelled = await queued(on: slots) { XCTFail("Cancelled image was admitted") }
        var admitted = false
        let next = await queued(on: slots) { admitted = true }
        cancelled.cancel()
        do {
            try await cancelled.value
            XCTFail("Expected cancellation")
        } catch is CancellationError {}
        XCTAssertFalse(admitted)
        slots.release()
        try await next.value
        XCTAssertTrue(admitted)
        slots.release()
        slots.release()
    }

    @MainActor
    private func queued(on slots: ConversationImageSlots, afterAdmission: @escaping @MainActor () -> Void) async -> Task<Void, any Error> {
        let (started, signal) = AsyncStream<Void>.makeStream()
        let task = Task { @MainActor in
            signal.yield(())
            signal.finish()
            // The test shares this actor and cannot resume until acquire suspends or returns.
            try await slots.acquire()
            defer { slots.release() }
            try Task.checkCancellation()
            afterAdmission()
        }
        for await _ in started { break }
        return task
    }

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

    func testOperatorUploadBoundaryExcludesCaptionReferences() {
        let caption = """
            截图 📷
            ![a](/srv/agent/media/inbox/caption-1.png)
            ![b](/srv/agent/media/inbox/caption-2.png)
            ![c](/srv/agent/media/inbox/caption-3.png)
            """
        let suffix = "\n\n" + """
            ![screenshot.png](/srv/agent/media/inbox/request%2Dtest%2D1%2Dpreview%2Epng)

            [document.pdf](/srv/agent/media/inbox/request%2Dtest%2D2%2Ddocument%2Epdf)

            ![second.png](/srv/agent/media/inbox/request%2Dtest%2D3%2Dsecond%2Epng)
            """
        let text = caption + suffix
        let expected: [ConversationImageReference] = [
            .init(reference: "/srv/agent/media/inbox/request-test-1-preview.png", label: "screenshot.png"),
            .init(reference: "/srv/agent/media/inbox/request-test-3-second.png", label: "second.png"),
        ]
        XCTAssertEqual(ConversationImageReference.controlAttachments(in: text, captionUTF8Bytes: Int64(caption.utf8.count)), expected)
        XCTAssertEqual(ConversationImageReference.controlAttachments(in: suffix, captionUTF8Bytes: 0), expected)
        XCTAssertEqual(ConversationImageReference.operatorTailReferences("legacy caption" + suffix), expected)
        XCTAssertTrue(ConversationImageReference.controlAttachments(in: caption, captionUTF8Bytes: Int64(caption.utf8.count)).isEmpty)
        XCTAssertTrue(ConversationImageReference.controlAttachments(in: text, captionUTF8Bytes: -1).isEmpty)
        XCTAssertTrue(ConversationImageReference.operatorTailReferences(text + "\n![remote](https://example.invalid/media/inbox/remote.png)").isEmpty)
    }

    func testOnlyCanonicalControlMessagesVerifyUploadedImages() {
        let text = "\n![upload](/srv/agent/media/inbox/upload.png)"
        func message(surface: String, metadata: JSONValue) -> OperatorInputPresentation.Message? {
            OperatorInputPresentation.message(.object([
                "id": .string("message"), "agent_id": .string("agent"), "kind": .string("operator_prompt"),
                "delivery_surface": .string(surface), "metadata": metadata,
                "body": .object(["type": .string("text"), "text": .string(text)]),
            ]), agentID: "agent", messageID: "message")
        }
        let boundary: JSONValue = .object(["control_prompt_caption_utf8_bytes": .integer(0)])
        XCTAssertEqual(message(surface: "http_control_prompt", metadata: boundary)?.uploadedImages?.count, 1)
        XCTAssertNil(message(surface: "cli_prompt", metadata: boundary)?.uploadedImages)
        XCTAssertNil(message(surface: "http_control_prompt", metadata: .null)?.uploadedImages)
    }

    func testThumbnailRejectsInvalidDataAndNonImageTypes() throws {
        let png = try XCTUnwrap(Data(base64Encoded:
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNQKDgAAAIEAVHqHS4xAAAAAElFTkSuQmCC"))
        let thumbnail = try ConversationImageThumbnail.decode(png, mediaType: "image/png")
        XCTAssertEqual(thumbnail.width, 1)
        XCTAssertEqual(thumbnail.height, 1)
        XCTAssertThrowsError(try ConversationImageThumbnail.decode(png, mediaType: "text/plain")) {
            XCTAssertEqual($0 as? FilesFailure, .invalidReference)
        }
        XCTAssertThrowsError(try ConversationImageThumbnail.decode(Data("not an image".utf8), mediaType: "image/png")) {
            XCTAssertEqual($0 as? FilesFailure, .invalidReference)
        }
    }

    func testValidPNGExceedingPixelDimensionsIsTooLargeEvenBelowByteLimit() throws {
        let canvas = try XCTUnwrap(CGContext(data: nil, width: 16_385, height: 4, bitsPerComponent: 8,
                                             bytesPerRow: 0, space: CGColorSpaceCreateDeviceGray(),
                                             bitmapInfo: CGImageAlphaInfo.none.rawValue))
        canvas.setFillColor(gray: 0.5, alpha: 1)
        canvas.fill(CGRect(x: 0, y: 0, width: 16_385, height: 4))
        let image = try XCTUnwrap(canvas.makeImage())
        let png = NSMutableData()
        let encoder = try XCTUnwrap(CGImageDestinationCreateWithData(png as CFMutableData, UTType.png.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(encoder, image, nil)
        XCTAssertTrue(CGImageDestinationFinalize(encoder))
        XCTAssertLessThan(png.length, ConversationImageThumbnail.maximumBytes)
        XCTAssertThrowsError(try ConversationImageThumbnail.decode(png as Data, mediaType: "image/png")) {
            XCTAssertEqual($0 as? FilesFailure, .tooLarge)
        }
    }
}
