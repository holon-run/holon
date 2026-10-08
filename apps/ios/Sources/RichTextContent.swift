import SwiftUI
import Textual

/// A host file reference stays opaque until the authorized server resolver reads it.
enum RichTextLink: Equatable {
    case external(URL), reference(String), unsupported

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
            reference.fragment = nil
            guard components.host?.isEmpty == false, components.port == nil,
                  let value = reference.string else { return .unsupported }
            return .reference(value)
        case "holon-path":
            guard components.host == nil, components.query == nil, components.fragment == nil,
                  let path = components.percentEncodedPath.removingPercentEncoding,
                  path.hasPrefix("/"), !path.contains("\0") else { return .unsupported }
            return .reference(path)
        default: return .unsupported
        }
    }

    static func literalPathURL(_ path: String) -> URL? {
        guard path.hasPrefix("/"), path.utf8.count <= 16_384, !path.contains("\0") else { return nil }
        var components = URLComponents()
        components.scheme = "holon-path"
        components.path = path
        return components.url
    }
}

/// Foundation parses Markdown; this adapter only controls links and passive attachments.
@MainActor
struct HolonMarkdownParser: MarkupParser {
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
            if run.link == nil, run.inlinePresentationIntent?.contains(.code) == true {
                let text = String(content[run.range].characters)
                if let url = URL(string: text), case .reference = RichTextLink.classify(url) {
                    content[run.range].link = url
                } else { content[run.range].link = RichTextLink.literalPathURL(text) }
            } else if run.link == nil {
                let text = String(content[run.range].characters)
                let pattern = try NSRegularExpression(pattern: "(?:workspace|file)://[^\\s<>]+")
                for match in pattern.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                    guard let range = Range(match.range, in: text),
                          let url = URL(string: String(text[range])), case .reference = RichTextLink.classify(url) else { continue }
                    let lower = content.index(run.range.lowerBound, offsetByCharacters: text.distance(from: text.startIndex, to: range.lowerBound))
                    let upper = content.index(lower, offsetByCharacters: text.distance(from: range.lowerBound, to: range.upperBound))
                    content[lower..<upper].link = url
                }
            }
        }
        return content
    }
}

struct RichTextContent: View {
    let text: String
    var openReference: ((String) -> Void)? = nil
    @State private var unsupportedLink = false

    var body: some View {
        StructuredText(text, parser: HolonMarkdownParser())
            .textual.structuredTextStyle(.gitHub)
            .textual.textSelection(.enabled)
            .textual.overflowMode(.wrap)
            .font(.body)
            .frame(maxWidth: .infinity, alignment: .leading)
            .environment(\.openURL, OpenURLAction { url in
                switch RichTextLink.classify(url) {
                case .external: return .systemAction
                case .reference(let reference):
                    if let openReference { openReference(reference) }
                    else { unsupportedLink = true }
                    return .handled
                case .unsupported: unsupportedLink = true; return .handled
                }
            })
            .alert("reading.linkUnavailable", isPresented: $unsupportedLink) {
                Button("action.ok", role: .cancel) {}
            }
    }
}
