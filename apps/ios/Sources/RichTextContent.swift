import SwiftUI
import Textual
import UIKit

/// A host file reference stays opaque until the authorized server resolver reads it.
enum RichTextLink: Equatable {
    case external(URL), reference(String), relative(String), unsupported

    static func classify(_ url: URL) -> Self {
        let value = url.absoluteString
        guard value.utf8.count <= 16_384, !value.contains("\0"),
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.user == nil, components.password == nil else { return .unsupported }
        switch components.scheme?.lowercased() {
        case "http", "https":
            return components.host?.isEmpty == false ? .external(url) : .unsupported
        case "mailto": return components.path.isEmpty ? .unsupported : .external(url)
        case "file":
            guard components.host == nil || components.host == "" || components.host == "localhost",
                  components.port == nil, components.query == nil,
                  let path = components.percentEncodedPath.removingPercentEncoding,
                  path.hasPrefix("/"), !path.contains("\0") else { return .unsupported }
            return .reference(path)
        case "workspace":
            // The SDK/server validate workspace/root membership; never reinterpret it as a device URL.
            var reference = components
            reference.scheme = "workspace"
            reference.fragment = nil
            guard components.host?.isEmpty == false, components.port == nil,
                  let value = reference.string else { return .unsupported }
            return .reference(value)
        case "holon-path":
            guard components.host == nil, components.query == nil, components.fragment == nil,
                  let path = components.percentEncodedPath.removingPercentEncoding,
                  path.hasPrefix("/"), !path.contains("\0") else { return .unsupported }
            return .reference(path)
        case nil:
            guard components.host == nil, components.query == nil,
                  let path = components.percentEncodedPath.removingPercentEncoding,
                  !path.isEmpty, !path.contains("\0") else { return .unsupported }
            if path.hasPrefix("/") { return path.hasPrefix("//") ? .unsupported : .reference(path) }
            return path.contains("\\") ? .unsupported : .relative(path)
        case "holon-relative":
            guard components.host == nil, components.query == nil,
                  let path = components.percentEncodedPath.removingPercentEncoding,
                  !path.isEmpty, !path.hasPrefix("/"), !path.contains("\\"), !path.contains("\0") else { return .unsupported }
            return .relative(path)
        default: return .unsupported
        }
    }

    static func literalPathURL(_ path: String) -> URL? {
        guard path.hasPrefix("/"), !path.hasPrefix("//"), path.utf8.count <= 16_384, !path.contains("\0") else { return nil }
        var components = URLComponents()
        components.scheme = "holon-path"
        components.path = path
        return components.url
    }

    static func literalRelativeURL(_ path: String) -> URL? {
        guard path.hasPrefix("./") || path.hasPrefix("../") else { return nil }
        return relativeURL(path)
    }

    static func relativeURL(_ path: String) -> URL? {
        guard !path.isEmpty, !path.hasPrefix("/"), path.utf8.count <= 16_384,
              !path.contains("\0"), !path.contains("\\") else { return nil }
        var components = URLComponents(); components.scheme = "holon-relative"; components.path = path
        return components.url
    }
}

/// Match Android's message-path boundaries. A bare absolute path is literal, not a URI.
enum MessagePathLinks {
    struct Match {
        let range: Range<String.Index>
        let url: URL
    }
    private static let pattern = try! NSRegularExpression(
        pattern: #"(?:workspace|file)://[^\s<>"')\]}，。；：]+|(?<![^\s：(\[{=，；])/(?!/)[^\s<>"')\]}，。；：]+"#,
        options: [.caseInsensitive])

    static func matches(_ text: String, preceding: Character? = nil) -> [Match] {
        pattern.matches(in: text, range: NSRange(text.startIndex..., in: text)).compactMap { match in
            guard var range = Range(match.range, in: text) else { return nil }
            let absolute = text[range.lowerBound] == "/"
            if absolute, range.lowerBound == text.startIndex, let preceding,
               !preceding.isWhitespace, !"：( [{=，；".contains(preceding) { return nil }
            while range.lowerBound < range.upperBound, ".,;!?:。；，！？".contains(text[text.index(before: range.upperBound)]) {
                range = range.lowerBound..<text.index(before: range.upperBound)
            }
            let candidate = String(text[range])
            guard candidate.count > 1 else { return nil }
            if absolute, let url = RichTextLink.literalPathURL(candidate) { return Match(range: range, url: url) }
            guard let url = URL(string: candidate), case .reference = RichTextLink.classify(url) else { return nil }
            return Match(range: range, url: url)
        }
    }

    static func annotate(_ content: inout AttributedString, range: Range<AttributedString.Index>) {
        let text = String(content[range].characters)
        let preceding: Character? = range.lowerBound == content.startIndex ? nil :
            content.characters[content.index(range.lowerBound, offsetByCharacters: -1)]
        for match in matches(text, preceding: preceding) {
            let lower = content.index(range.lowerBound, offsetByCharacters: text.distance(from: text.startIndex, to: match.range.lowerBound))
            let upper = content.index(lower, offsetByCharacters: text.distance(from: match.range.lowerBound, to: match.range.upperBound))
            content[lower..<upper].link = match.url
        }
    }
}

/// Foundation parses Markdown; this adapter only controls links and passive attachments.
@MainActor
struct HolonMarkdownParser: MarkupParser {
    var allowsRelativePaths = false
    func attributedString(for input: String) throws -> AttributedString {
        var content = try AttributedString(markdown: input, options: .init(allowsExtendedAttributes: false))
        for run in Array(content.runs) {
            if let image = run.imageURL {
                content[run.range].imageURL = nil
                content[run.range].link = image
            }
            let blockCode = run.presentationIntent?.components.contains {
                if case .codeBlock = $0.kind { return true }
                return false
            } == true
            if blockCode { content[run.range].link = nil; continue }
            if let link = run.link, link.scheme == nil {
                // Give relative links an explicit app-routed scheme. This is
                // still an opaque host reference, never a device file URL.
                if case .reference(let path) = RichTextLink.classify(link) {
                    content[run.range].link = RichTextLink.literalPathURL(path)
                } else if case .relative(let path) = RichTextLink.classify(link) {
                    content[run.range].link = RichTextLink.relativeURL(path)
                } else { content[run.range].link = URL(string: "holon-unsupported:relative") }
            }
            if run.link == nil, run.inlinePresentationIntent?.contains(.code) == true {
                let text = String(content[run.range].characters)
                if text.hasPrefix("/") {
                    content[run.range].link = RichTextLink.literalPathURL(text)
                } else if let url = URL(string: text), case .reference = RichTextLink.classify(url) {
                    content[run.range].link = url
                } else {
                    content[run.range].link = RichTextLink.literalPathURL(text) ??
                        (allowsRelativePaths ? RichTextLink.literalRelativeURL(text) : nil)
                }
            } else if run.link == nil {
                MessagePathLinks.annotate(&content, range: run.range)
            }
        }
        // Foundation retains GFM task markers as text. Change only native list-item prefixes.
        var seenListItems: Set<Int> = []
        let taskMarkers = content.runs.compactMap { run -> (Int, Bool)? in
            guard let item = run.presentationIntent?.components.first(where: {
                if case .listItem = $0.kind { return true }; return false
            }), seenListItems.insert(item.identity).inserted else { return nil }
            let code = run.presentationIntent?.components.contains {
                if case .codeBlock = $0.kind { return true }; return false
            } == true || run.inlinePresentationIntent?.contains(.code) == true
            let value = String(content[run.range].characters)
            guard !code, run.inlinePresentationIntent == nil || run.inlinePresentationIntent?.isEmpty == true,
                  run.link == nil, run.imageURL == nil,
                  value.hasPrefix("[x] ") || value.hasPrefix("[X] ") || value.hasPrefix("[ ] ") else { return nil }
            return (content.characters.distance(from: content.startIndex, to: run.range.lowerBound), !value.hasPrefix("[ ] "))
        }
        for (offset, checked) in taskMarkers.reversed() {
            let lower = content.index(content.startIndex, offsetByCharacters: offset)
            let upper = content.index(lower, offsetByCharacters: 4)
            var marker = AttributedString(checked ? "☑ " : "☐ ")
            if let attributes = content[lower..<upper].runs.first?.attributes { marker.mergeAttributes(attributes) }
            content.replaceSubrange(lower..<upper, with: marker)
        }
        return content
    }
}

/// Operator input stays verbatim; only authorized host-file actions are annotated.
struct OperatorMessageText: View {
    let text: String
    @Environment(\.holonOpenReference) private var openReference
    @State private var unavailable = false
    var body: some View {
        Text(Self.attributed(text)).textSelection(.enabled)
            .environment(\.openURL, OpenURLAction { url in
                if case .reference(let reference) = RichTextLink.classify(url), let openReference {
                    openReference(reference)
                } else { unavailable = true }
                return .handled
            })
            .alert("reading.linkUnavailable", isPresented: $unavailable) {
                Button("action.ok", role: .cancel) {}
            }
    }
    static func attributed(_ text: String) -> AttributedString {
        var content = AttributedString(text)
        MessagePathLinks.annotate(&content, range: content.startIndex..<content.endIndex)
        return content
    }
}

struct RichTextContent: View {
    let text: String
    var openReference: ((String) -> Void)? = nil
    var openRelative: ((String) -> Void)? = nil
    var onReport: (() -> Void)? = nil
    @State private var unsupportedLink = false
    @State private var selectingText = false

    var body: some View {
        StructuredText(text, parser: HolonMarkdownParser(allowsRelativePaths: openRelative != nil))
            .textual.structuredTextStyle(.gitHub)
            // Textual's UIKit selection overlay swallows link taps on iOS 26.
            // Keep native links; range selection has its own system text surface.
            .textual.textSelection(.disabled)
            .textual.overflowMode(.wrap)
            .font(.body)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            // Keep fragment and selection measurements in one coordinate group.
            .geometryGroup()
            .environment(\.openURL, OpenURLAction { url in
                switch RichTextLink.classify(url) {
                case .external: return .systemAction
                case .reference(let reference):
                    if let openReference { openReference(reference) }
                    else { unsupportedLink = true }
                    return .handled
                case .relative(let path):
                    if let openRelative { openRelative(path) } else { unsupportedLink = true }
                    return .handled
                case .unsupported: unsupportedLink = true; return .handled
                }
            })
            .alert("reading.linkUnavailable", isPresented: $unsupportedLink) {
                Button("action.ok", role: .cancel) {}
            }
            .contextMenu {
                Button("reading.selectText", systemImage: "text.cursor") { selectingText = true }
                if let onReport { Button("report.title", systemImage: "flag") { onReport() } }
            }
            .accessibilityAction(named: Text("reading.selectText")) { selectingText = true }
            .sheet(isPresented: $selectingText) {
                NavigationStack {
                    RichTextSelectionView(text: text)
                    .navigationTitle("reading.selectText").navigationBarTitleDisplayMode(.inline)
                    .toolbar { Button("files.dismiss") { selectingText = false } }
                }
            }
            .onChange(of: text) { _, _ in selectingText = false }
            .onDisappear { selectingText = false }
    }

}

/// Select verbatim source without Markdown links, detectors or an editable draft.
struct RichTextSelectionView: UIViewRepresentable {
    let text: String
    func makeUIView(context: Context) -> UITextView { Self.makeTextView(text: text) }
    func updateUIView(_ view: UITextView, context: Context) {
        if view.text != text { view.text = text }
        view.font = .preferredFont(forTextStyle: .body)
    }
    static func makeTextView(text: String) -> UITextView {
        let view = UITextView()
        view.isEditable = false; view.isSelectable = true
        view.dataDetectorTypes = []
        view.font = .preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.textColor = .label; view.backgroundColor = .systemBackground
        view.textContainerInset = .init(top: 16, left: 12, bottom: 16, right: 12)
        view.accessibilityIdentifier = "reading.selectionText"
        view.text = text
        return view
    }
}
