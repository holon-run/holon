import Foundation
import ImageIO
import SwiftUI
import UniformTypeIdentifiers

struct ConversationImageReference: Hashable, Sendable {
    let reference: String
    let label: String

    static let inlineLimit = 3

    private static func attachmentURL(_ url: URL) -> String? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.query == nil || components.scheme?.lowercased() == "workspace",
              components.fragment == nil,
              case .reference(let reference) = RichTextLink.classify(url) else { return nil }
        return reference
    }

    static func markdown(_ text: String) -> [Self] {
        guard text.contains("!["), let parsed = try? AttributedString(markdown: text) else { return [] }
        var images: [Self] = []
        var seen = Set<String>()
        for run in parsed.runs {
            guard let url = run.imageURL, let reference = attachmentURL(url),
                  seen.insert(reference).inserted else { continue }
            images.append(.init(reference: reference, label: String(parsed[run.range].characters)))
            if images.count == inlineLimit { break }
        }
        return images
    }

    // With legacy history, this finds opt-in references, not verified uploads.
    static func operatorTailReferences(_ text: String) -> [Self] {
        guard text.contains("!["), text.contains("/media/inbox/") else { return [] }
        var images: [Self] = []
        var seen = Set<String>()
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        for line in lines.reversed() {
            // Control puts a blank line between consecutive attachments.
            if line.isEmpty { continue }
            let image = line.hasPrefix("![")
            guard (image || line.hasPrefix("[")), line.hasSuffix(")"), let boundary = line.range(of: "]("),
                  let url = URL(string: String(line[boundary.upperBound..<line.index(before: line.endIndex)])),
                  url.scheme == nil, let reference = attachmentURL(url),
                  reference.hasPrefix("/"), !reference.contains("\\"),
                  (reference as NSString).deletingLastPathComponent.hasSuffix("/media/inbox") else { break }
            guard image, seen.insert(reference).inserted else { continue }
            images.append(.init(reference: reference, label: String(line[line.index(line.startIndex, offsetBy: 2)..<boundary.lowerBound])))
            if images.count == inlineLimit { break }
        }
        return images.reversed()
    }

    static func controlAttachments(in text: String, captionUTF8Bytes: Int64) -> [Self] {
        let utf8 = text.utf8
        guard let length = Int(exactly: captionUTF8Bytes), length >= 0,
              let split = utf8.index(utf8.startIndex, offsetBy: length, limitedBy: utf8.endIndex),
              split != utf8.endIndex, utf8[split] == 0x0A else { return [] }
        return operatorTailReferences(String(decoding: utf8[split...], as: UTF8.self))
    }
}

struct ConversationImageAccess {
    let scope: Int?
    let load: @MainActor (String, Int) async throws -> CGImage
}

private struct ConversationImageAccessKey: EnvironmentKey {
    static var defaultValue: ConversationImageAccess? { nil }
}

extension EnvironmentValues {
    var holonConversationImageAccess: ConversationImageAccess? {
        get { self[ConversationImageAccessKey.self] }
        set { self[ConversationImageAccessKey.self] = newValue }
    }
}

struct ConversationImagePreviews: View {
    let images: [ConversationImageReference]

    var body: some View {
        ForEach(images, id: \.reference) { reference in
            ConversationImagePreview(reference: reference)
        }
    }
}

@MainActor
final class ConversationImageSlots {
    private var available: Int
    private let maximumWaiting: Int
    private var waiters: [(UUID, CheckedContinuation<Void, any Error>)] = []

    init(active: Int = 3, waiting: Int = 12) {
        available = active
        maximumWaiting = waiting
    }

    func acquire() async throws {
        try Task.checkCancellation()
        if available > 0 {
            available -= 1
            return
        }
        let id = UUID()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (waiter: CheckedContinuation<Void, any Error>) in
                if Task.isCancelled { waiter.resume(throwing: CancellationError()) }
                else if waiters.count >= maximumWaiting { waiter.resume(throwing: ConversationPreviewDeferred()) }
                else { waiters.append((id, waiter)) }
            }
        } onCancel: {
            Task { @MainActor in
                guard let index = self.waiters.firstIndex(where: { $0.0 == id }) else { return }
                self.waiters.remove(at: index).1.resume(throwing: CancellationError())
            }
        }
    }

    func release() {
        // A resumed waiter owns the slot, even if it was cancelled during handoff.
        if waiters.isEmpty { available += 1 }
        else { waiters.removeFirst().1.resume() }
    }

    func cancelWaiting() {
        let cancelled = waiters
        waiters.removeAll()
        for (_, waiter) in cancelled { waiter.resume(throwing: CancellationError()) }
    }
}

struct ConversationPreviewDeferred: Error {}

/// Coalesce only in-flight automatic checks; don't retain full message bodies.
@MainActor
final class ConversationImageDetails {
    private static let activeLimit = 2
    private static let waitingLimit = 8
    private let slots = ConversationImageSlots(active: activeLimit, waiting: waitingLimit)
    private struct Job {
        let id: UUID
        let task: Task<OperatorInputPresentation.Message, any Error>
        var readers: Set<UUID> = []
    }
    private var jobs: [String: Job] = [:]

    func load(_ id: String, operation: @escaping @MainActor () async throws -> OperatorInputPresentation.Message)
        async throws -> OperatorInputPresentation.Message {
        try Task.checkCancellation()
        if jobs[id] == nil {
            guard jobs.count < Self.activeLimit + Self.waitingLimit else { throw ConversationPreviewDeferred() }
            let jobID = UUID()
            let task = Task {
                defer { if self.jobs[id]?.id == jobID { self.jobs.removeValue(forKey: id) } }
                try await self.slots.acquire()
                defer { self.slots.release() }
                try Task.checkCancellation()
                let message = try await operation()
                try Task.checkCancellation()
                return message
            }
            jobs[id] = Job(id: jobID, task: task)
        }
        guard var job = jobs[id] else { throw CancellationError() }
        let reader = UUID()
        job.readers.insert(reader)
        jobs[id] = job
        let jobID = job.id, task = job.task
        return try await withTaskCancellationHandler {
            let message = try await task.value
            try Task.checkCancellation()
            return message
        } onCancel: {
            Task { @MainActor in self.removeReader(reader, from: id, jobID: jobID) }
        }
    }

    private func removeReader(_ reader: UUID, from id: String, jobID: UUID) {
        guard var job = jobs[id], job.id == jobID else { return }
        job.readers.remove(reader)
        if job.readers.isEmpty {
            jobs.removeValue(forKey: id)
            job.task.cancel()
        } else {
            jobs[id] = job
        }
    }

    func cancelAll() {
        for job in jobs.values { job.task.cancel() }
        jobs.removeAll()
        slots.cancelWaiting()
    }
}

private struct ConversationImagePreview: View {
    private struct Request: Hashable {
        let scope: Int?
        let reference: ConversationImageReference
        let retry: Int
    }
    private enum Result { case image(CGImage), tooLarge, unavailable, deferred }

    let reference: ConversationImageReference
    @Environment(\.holonConversationImageAccess) private var access
    @Environment(\.holonOpenReference) private var openReference
    @State private var response: (Request, Result)?
    @State private var retry = 0

    var body: some View {
        let request = Request(scope: access?.scope, reference: reference, retry: retry)
        Group {
            switch response?.0 == request ? response?.1 : nil {
            case .image(let image):
                Image(image, scale: 1, label: reference.label.isEmpty ? Text("reading.image") : Text(verbatim: reference.label))
                    .resizable().scaledToFit()
                    .accessibilityIdentifier("conversation.image.loaded")
            case .tooLarge:
                VStack(spacing: 4) {
                    Text("reading.imageTooLarge")
                    if let openReference {
                        Button("reading.imageOpenInFiles") { openReference(reference.reference) }
                            .accessibilityIdentifier("conversation.image.openInFiles")
                    }
                }
                .font(.callout).multilineTextAlignment(.center)
            case .unavailable:
                Button("reading.imageUnavailable", systemImage: "photo.badge.exclamationmark") { retry &+= 1 }
                    .font(.callout).multilineTextAlignment(.center)
                    .accessibilityIdentifier("conversation.image.unavailable")
            case .deferred:
                Button("reading.imageLoadPreview", systemImage: "photo") { retry &+= 1 }
                    .font(.callout).multilineTextAlignment(.center)
            case nil:
                if request.scope == nil {
                    Label("reading.imageOffline", systemImage: "photo")
                        .font(.callout).multilineTextAlignment(.center)
                } else {
                    ProgressView("reading.imageLoading").font(.caption)
                }
            }
        }
        .frame(maxWidth: 280)
        .frame(height: 140)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
        .task(id: request) {
            response = nil
            guard let access, let scope = request.scope else { return }
            do {
                let image = try await access.load(reference.reference, scope)
                try Task.checkCancellation()
                response = (request, .image(image))
            } catch {
                guard !Task.isCancelled, !(error is CancellationError) else { return }
                let result: Result = error is ConversationPreviewDeferred ? .deferred :
                    ((error as? FilesFailure) == .tooLarge ? .tooLarge : .unavailable)
                response = (request, result)
            }
        }
    }
}

enum ConversationImageThumbnail {
    static let maximumBytes = 5 * 1_024 * 1_024
    static let maximumPixelSize = 768

    static func decode(_ data: Data, mediaType: String?) throws -> CGImage {
        guard data.count <= maximumBytes else { throw FilesFailure.tooLarge }
        guard !data.isEmpty else { throw FilesFailure.invalidReference }
        if let type = mediaType?.split(separator: ";").first?.lowercased(),
           !type.hasPrefix("image/"), type != "application/octet-stream" { throw FilesFailure.invalidReference }
        let sourceOptions = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(data as CFData, sourceOptions),
              let identifier = CGImageSourceGetType(source) as String?,
              let type = UTType(identifier),
              [.png, .jpeg, .gif, .heic, .heif, .webP].contains(type),
              CGImageSourceGetStatus(source) == .statusComplete,
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              width > 0, height > 0 else { throw FilesFailure.invalidReference }
        guard width <= 16_384, height <= 16_384, width * height <= 48_000_000 else {
            throw FilesFailure.tooLarge
        }
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: maximumPixelSize
        ] as CFDictionary) else { throw FilesFailure.invalidReference }
        return image
    }
}
