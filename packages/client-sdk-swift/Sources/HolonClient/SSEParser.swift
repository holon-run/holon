import Foundation

public struct HolonSSEEvent: Equatable, Sendable {
    public let event: String
    public let id: String?
    public let data: String
}

/// Incremental parser bounded by raw frame bytes, including complete line endings.
/// Call finish() at normal EOF; it never commits an unterminated frame.
public struct HolonSSEParser: Sendable {
    private let maximumFrameBytes: Int
    private var line: [UInt8] = []
    private var pendingCR = false
    private var firstLine = true
    private var event = ""
    private var id: String?
    private var data: [String] = []
    private var frameBytes = 0

    public init(maximumFrameBytes: Int = 1_048_576) throws {
        guard maximumFrameBytes > 0 else { throw HolonClientError.invalidRequest }
        self.maximumFrameBytes = maximumFrameBytes
    }

    public mutating func append(_ byte: UInt8) throws -> HolonSSEEvent? {
        var completed: HolonSSEEvent?
        if pendingCR {
            pendingCR = false
            if byte != 10 { completed = dispatchLine() }
        }
        frameBytes += 1
        guard frameBytes <= maximumFrameBytes else { throw HolonClientError.streamLimitExceeded }
        switch byte {
        case 10: return dispatchLine()
        case 13: pendingCR = true
        default: line.append(byte)
        }
        return completed
    }

    /// Completes a deferred CR line ending, not an unterminated line.
    public mutating func finish() -> HolonSSEEvent? {
        guard pendingCR else { return nil }
        pendingCR = false
        return dispatchLine()
    }

    private mutating func dispatchLine() -> HolonSSEEvent? {
        var text = String(decoding: line, as: UTF8.self)
        line.removeAll(keepingCapacity: true)
        if firstLine {
            firstLine = false
            if text.hasPrefix("\u{FEFF}") { text.removeFirst() }
        }
        if text.isEmpty {
            let result = data.isEmpty ? nil :
                HolonSSEEvent(event: event.isEmpty ? "message" : event, id: id,
                              data: data.joined(separator: "\n"))
            event = ""
            data.removeAll(keepingCapacity: true)
            frameBytes = 0
            return result
        }
        if text.hasPrefix(":") { return nil }
        let pieces = text.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
        let field = String(pieces[0])
        var value = pieces.count == 2 ? String(pieces[1]) : ""
        if value.hasPrefix(" ") { value.removeFirst() }
        switch field {
        case "event": event = value
        case "id": if !value.contains("\0") { id = value }
        case "data": data.append(value)
        default: break
        }
        return nil
    }
}

/// Owns exactly one stream. The caller explicitly closes or reopens it.
public final class HolonEventStream: AsyncSequence, Sendable {
    public typealias Element = HolonResponse<HolonSSEEvent>
    public typealias AsyncIterator = AsyncThrowingStream<Element, any Error>.Iterator
    private let events: AsyncThrowingStream<Element, any Error>
    private let stop: @Sendable () -> Void

    init(events: AsyncThrowingStream<Element, any Error>, stop: @escaping @Sendable () -> Void) {
        self.events = events
        self.stop = stop
    }

    public func makeAsyncIterator() -> AsyncIterator { events.makeAsyncIterator() }
    public func close() { stop() }
    deinit { stop() }
}
