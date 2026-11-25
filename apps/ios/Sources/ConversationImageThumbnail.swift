import Foundation
import ImageIO
import SwiftUI
import UniformTypeIdentifiers

struct ConversationImageReference: Hashable {
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

    // Control sends append standalone image references to the operator's plain text.
    // Keep all other operator Markdown literal, including arbitrary image syntax.
    static func operatorAttachments(_ text: String) -> [Self] {
        guard text.contains("\n!["), text.contains("/media/inbox/") else { return [] }
        var images: [Self] = []
        var seen = Set<String>()
        for line in text.split(separator: "\n") {
            guard line.hasPrefix("!["), line.hasSuffix(")"), let boundary = line.range(of: "]("),
                  let url = URL(string: String(line[boundary.upperBound..<line.index(before: line.endIndex)])),
                  url.scheme == nil, let reference = attachmentURL(url),
                  reference.hasPrefix("/"), !reference.contains("\\"),
                  (reference as NSString).deletingLastPathComponent.hasSuffix("/media/inbox"),
                  seen.insert(reference).inserted else { continue }
            images.append(.init(reference: reference, label: String(line[line.index(line.startIndex, offsetBy: 2)..<boundary.lowerBound])))
            if images.count == inlineLimit { break }
        }
        return images
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

private struct ConversationImagePreview: View {
    private struct Request: Hashable {
        let scope: Int?
        let reference: ConversationImageReference
        let retry: Int
    }
    private enum Result { case image(CGImage), unavailable }

    let reference: ConversationImageReference
    @Environment(\.holonConversationImageAccess) private var access
    @State private var response: (Request, Result)?
    @State private var retry = 0

    var body: some View {
        let request = Request(scope: access?.scope, reference: reference, retry: retry)
        Group {
            if case .image(let image) = response?.0 == request ? response?.1 : nil {
                Image(image, scale: 1, label: reference.label.isEmpty ? Text("reading.image") : Text(verbatim: reference.label))
                    .resizable().scaledToFit()
                    .accessibilityIdentifier("conversation.image.loaded")
            } else if request.scope == nil || response?.0 == request {
                if request.scope != nil {
                    Button("reading.imageUnavailable", systemImage: "photo.badge.exclamationmark") { retry &+= 1 }
                        .font(.callout).multilineTextAlignment(.center)
                        .accessibilityIdentifier("conversation.image.unavailable")
                } else {
                    Label("reading.imageOffline", systemImage: "photo")
                        .font(.callout).multilineTextAlignment(.center)
                }
            } else {
                ProgressView("reading.imageLoading").font(.caption)
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
                guard !Task.isCancelled else { return }
                response = (request, .unavailable)
            }
        }
    }
}

enum ConversationImageThumbnail {
    static let maximumBytes = 5 * 1_024 * 1_024
    static let maximumPixelSize = 768

    static func decode(_ data: Data, mediaType: String?) -> CGImage? {
        guard !data.isEmpty, data.count <= maximumBytes else { return nil }
        if let type = mediaType?.split(separator: ";").first?.lowercased(),
           !type.hasPrefix("image/"), type != "application/octet-stream" { return nil }
        let sourceOptions = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(data as CFData, sourceOptions),
              let identifier = CGImageSourceGetType(source) as String?,
              let type = UTType(identifier),
              [.png, .jpeg, .gif, .heic, .heif, .webP].contains(type),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              (1...16_384).contains(width), (1...16_384).contains(height),
              width * height <= 48_000_000 else { return nil }
        return CGImageSourceCreateThumbnailAtIndex(source, 0, [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: maximumPixelSize
        ] as CFDictionary)
    }
}
