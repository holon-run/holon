import Foundation
import ImageIO
import Observation
import UIKit
import HolonClient

/// Session-only decoded PNGs. A cache hit is decorative data, never proof of access.
@Observable @MainActor
final class AgentAvatarStore {
    struct Key: Hashable {
        let agentID: String
        let expression: HolonAvatarExpression
    }
    private struct Entry {
        let image: UIImage
        let bytes: Int
    }
    private enum Capability { case unknown, checking, supported, unsupported, failed }

    private(set) var sessionID = UUID()
    private(set) var retryID = 0
    private(set) var isOnline = false
    private var entries: [Key: Entry] = [:]
    private(set) var retainedBytes = 0
    var cachedCount: Int { entries.count }
    var inFlightCount: Int { active.count }
    @ObservationIgnored var onAuthorizationFailure: ((HolonHTTPFailure) -> Void)?
    @ObservationIgnored private var transport: (any AgentAvatarTransport)?
    @ObservationIgnored private var operationID = UUID()
    @ObservationIgnored private var capability: Capability = .unknown
    @ObservationIgnored private var negotiation: Task<Void, Never>?
    @ObservationIgnored private var active: [Key: Task<Void, Never>] = [:]
    @ObservationIgnored private var pending: [Key] = []
    @ObservationIgnored private var failed: Set<Key> = []
    @ObservationIgnored private var access: [Key: UInt64] = [:]
    @ObservationIgnored private var clock: UInt64 = 0
    @ObservationIgnored private let entryLimit: Int
    @ObservationIgnored private let byteLimit: Int
    @ObservationIgnored private let concurrency: Int

    init(entryLimit: Int = 128, byteLimit: Int = 8 * 1_024 * 1_024, concurrency: Int = 4) {
        self.entryLimit = min(128, max(1, entryLimit))
        self.byteLimit = min(8 * 1_024 * 1_024, max(1, byteLimit))
        self.concurrency = min(4, max(1, concurrency))
    }

    func activate(transport: any AgentAvatarTransport) {
        disconnect()
        self.transport = transport
    }

    func disconnect() {
        cancelRequests()
        transport = nil
        sessionID = UUID()
        isOnline = false
        capability = .unknown
        entries.removeAll()
        access.removeAll()
        failed.removeAll()
        retainedBytes = 0
    }

    func setOnline(_ online: Bool) {
        guard isOnline != online else { return }
        isOnline = online
        if online { pump() } else { cancelRequests() }
    }

    func invalidateResources() {
        guard let transport else { disconnect(); return }
        let online = isOnline
        activate(transport: transport)
        setOnline(online)
    }

    /// Only explicit reader refresh retries failures; scrolling cannot create a retry loop.
    func retry() {
        failed.removeAll()
        if capability == .failed { capability = .unknown }
        retryID &+= 1
        pump()
    }

    func image(agentID: String, expression: HolonAvatarExpression) -> UIImage? {
        let key = Key(agentID: agentID, expression: isOnline ? expression : .neutral)
        guard let entry = entries[key] else { return nil }
        clock &+= 1
        access[key] = clock
        return entry.image
    }

    func request(agentID: String, expression: HolonAvatarExpression) {
        guard isOnline, transport != nil, capability != .unsupported, failed.count < 512,
              !agentID.isEmpty, agentID.utf8.count <= 512 else { return }
        // Always fetch a neutral version first for this session's offline reading.
        for value in [.neutral, expression] as [HolonAvatarExpression] {
            let key = Key(agentID: agentID, expression: value)
            guard entries[key] == nil, active[key] == nil, !failed.contains(key), !pending.contains(key) else { continue }
            // Bounded pending work, even when a large roster is scrolled during negotiation.
            if pending.count == 128 { pending.removeFirst() }
            pending.append(key)
        }
        pump()
    }

    private func cancelRequests() {
        operationID = UUID()
        negotiation?.cancel()
        negotiation = nil
        if capability == .checking { capability = .unknown }
        for task in active.values { task.cancel() }
        active.removeAll()
        pending.removeAll()
    }

    private func current(_ stamp: UUID) -> Bool {
        operationID == stamp && isOnline && !Task.isCancelled
    }

    private func pump() {
        guard isOnline, let transport else { return }
        if capability == .unknown {
            capability = .checking
            let stamp = operationID
            negotiation = Task { [weak self] in
                do {
                    let supported = try await transport.supportsAvatars()
                    guard let self, self.current(stamp) else { return }
                    self.negotiation = nil
                    self.capability = supported ? .supported : .unsupported
                    if !supported { self.pending.removeAll() }
                    self.pump()
                } catch {
                    guard let self, self.current(stamp) else { return }
                    self.negotiation = nil
                    self.capability = .failed
                    self.pending.removeAll()
                    self.handleAuthorization(error)
                }
            }
        }
        guard capability == .supported else { return }
        while active.count < concurrency, failed.count < 512, !pending.isEmpty {
            let key = pending.removeFirst()
            guard entries[key] == nil, !failed.contains(key) else { continue }
            let stamp = operationID
            active[key] = Task { [weak self] in
                do {
                    let data = try await transport.image(agentID: key.agentID, expression: key.expression)
                    guard let self, self.current(stamp) else { return }
                    guard let entry = Self.decode(data), entry.bytes <= self.byteLimit else {
                        throw HolonClientError.malformedResponse
                    }
                    self.active.removeValue(forKey: key)
                    self.insert(entry, for: key)
                    self.pump()
                } catch {
                    guard let self, self.current(stamp) else { return }
                    self.active.removeValue(forKey: key)
                    // The failure ledger is bounded independently of the image cache.
                    if self.failed.count < 512 { self.failed.insert(key) }
                    self.handleAuthorization(error)
                    self.pump()
                }
            }
        }
    }

    private func handleAuthorization(_ error: any Error) {
        guard let failure = error as? HolonHTTPFailure, [401, 403].contains(failure.statusCode) else { return }
        disconnect()
        onAuthorizationFailure?(failure)
    }

    private func insert(_ entry: Entry, for key: Key) {
        while entries.count >= entryLimit || retainedBytes + entry.bytes > byteLimit {
            guard let oldest = access.min(by: { $0.value < $1.value })?.key,
                  let removed = entries.removeValue(forKey: oldest) else { return }
            retainedBytes -= removed.bytes
            access.removeValue(forKey: oldest)
        }
        clock &+= 1
        access[key] = clock
        entries[key] = entry
        retainedBytes += entry.bytes
    }

    private static func decode(_ data: Data) -> Entry? {
        guard data.count <= 2 * 1_024 * 1_024,
              let source = CGImageSourceCreateWithData(data as CFData, nil),
              CGImageSourceGetType(source) as String? == "public.png",
              CGImageSourceGetCount(source) == 1,
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue == 128,
              (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue == 128,
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
              let bitmap = CGContext(data: nil, width: 128, height: 128, bitsPerComponent: 8,
                                     bytesPerRow: 128 * 4, space: CGColorSpaceCreateDeviceRGB(),
                                     bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        // Detach from the source provider: retained images cannot retain arbitrary PNG metadata.
        bitmap.draw(image, in: CGRect(x: 0, y: 0, width: 128, height: 128))
        guard let decoded = bitmap.makeImage() else { return nil }
        return Entry(image: UIImage(cgImage: decoded), bytes: 128 * 128 * 4)
    }
}
