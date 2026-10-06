import Foundation
import HolonClient
import XCTest

final class SSEParserTests: XCTestCase {
    private func parse(_ text: String, maximum: Int = 1_048_576) throws -> [HolonSSEEvent] {
        var parser = try HolonSSEParser(maximumFrameBytes: maximum)
        var events = try text.utf8.compactMap { try parser.append($0) }
        if let event = parser.finish() { events.append(event) }
        return events
    }

    func testCRLFCommentsBOMMultilineAndUnknownFields() throws {
        let events = try parse("\u{FEFF}: hi\r\nretry: 1000\r\nid: epoch:1\r\n" +
                               "event: checkpoint\r\ndata: {\"a\":1}\r\ndata: second\r\n\r\n")
        XCTAssertEqual(events.count, 1)
        XCTAssertEqual(events[0].event, "checkpoint")
        XCTAssertEqual(events[0].id, "epoch:1")
        XCTAssertEqual(events[0].data, "{\"a\":1}\nsecond")
    }

    func testIDsPersistResetAndRejectNUL() throws {
        let events = try parse("id: 1\ndata: a\n\nid: bad\0id\ndata: b\n\nid:\ndata: c\n\n")
        XCTAssertEqual(events.map(\.id), ["1", "1", ""])
    }

    func testEOFNeverPublishesAnIncompleteEvent() throws {
        XCTAssertTrue(try parse("data: partial\nid: 9\n").isEmpty)
        XCTAssertTrue(try parse("data: partial").isEmpty)
        XCTAssertTrue(try parse(": keep-alive\n\n").isEmpty)
        XCTAssertEqual(try parse("data:\n\n").map(\.data), [""])
        XCTAssertEqual(try parse("event: future\ndata: 原文🙂\r\r").map(\.data), ["原文🙂"])
    }

    func testFrameLimitResetsOnlyAtBoundary() throws {
        XCTAssertThrowsError(try parse("data: \(String(repeating: "x", count: 40))\n\n", maximum: 20)) {
            XCTAssertEqual($0 as? HolonClientError, .streamLimitExceeded)
        }
        XCTAssertEqual(try parse("data: a\n\ndata: b\n\n", maximum: 10).count, 2)
        XCTAssertThrowsError(try HolonSSEParser(maximumFrameBytes: 0))
    }

    func testExactRawFrameLimitsForEveryLineEnding() throws {
        for ending in ["\n", "\r", "\r\n"] {
            let frame = "data: a\(ending)\(ending)"
            let byteCount = frame.utf8.count
            XCTAssertEqual(try parse(frame, maximum: byteCount).map(\.data), ["a"])
            XCTAssertThrowsError(try parse(frame, maximum: byteCount - 1)) {
                XCTAssertEqual($0 as? HolonClientError, .streamLimitExceeded)
            }
            XCTAssertEqual(try parse(frame + frame, maximum: byteCount).count, 2)
        }
    }

    func testTrailingLFIsCheckedBeforePublishingCRLFEvent() throws {
        var parser = try HolonSSEParser(maximumFrameBytes: 10)
        for byte in "data: a\r\n\r".utf8 {
            XCTAssertNil(try parser.append(byte))
        }
        XCTAssertThrowsError(try parser.append(10)) {
            XCTAssertEqual($0 as? HolonClientError, .streamLimitExceeded)
        }
    }

    func testCRLFBudgetAcrossEveryChunkBoundary() throws {
        let bytes = Array("data: a\r\n\r\ndata: b\r\n\r\n".utf8)
        for split in 0...bytes.count {
            var parser = try HolonSSEParser(maximumFrameBytes: 11)
            var events: [HolonSSEEvent] = []
            for chunk in [bytes[..<split], bytes[split...]] {
                for byte in chunk {
                    if let event = try parser.append(byte) { events.append(event) }
                }
            }
            XCTAssertNil(parser.finish())
            XCTAssertEqual(events.map(\.data), ["a", "b"])
        }
    }

    func testEOFCompletesOnlyDeferredCRLineEndings() throws {
        XCTAssertTrue(try parse("data: a\r").isEmpty)
        XCTAssertTrue(try parse("data: a\nid: partial").isEmpty)
        var parser = try HolonSSEParser(maximumFrameBytes: 9)
        for byte in "data: a\r\r".utf8 { XCTAssertNil(try parser.append(byte)) }
        XCTAssertEqual(parser.finish()?.data, "a")
        XCTAssertNil(parser.finish())
    }
}
