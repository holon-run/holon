import Foundation

struct FileTextIndex: Sendable {
    static let pageBytes = 16_384
    static let maximumBytes = 16 * 1024 * 1024
    let pages: [Range<Int>]
    let byteCount: Int

    /// Bounded disk reads, strict UTF-8, no whole-file String even for a long single line.
    static func build(url: URL) throws -> Self {
        let file = try FileHandle(forReadingFrom: url)
        defer { try? file.close() }
        let size = try file.seekToEnd()
        guard size <= maximumBytes else { throw FilesFailure.tooLarge }
        var pages: [Range<Int>] = [], offset = 0
        while offset < size {
            try Task.checkCancellation()
            try file.seek(toOffset: UInt64(offset))
            let data = try file.read(upToCount: pageBytes + 4) ?? Data()
            guard !data.isEmpty else { throw FilesFailure.unavailable }
            var length = min(pageBytes, data.count)
            if length < data.count {
                while length > 0, data[length] & 0xC0 == 0x80 { length -= 1 }
            }
            if let newline = data.prefix(length).lastIndex(of: 10), newline >= pageBytes / 4 {
                length = newline + 1
            }
            guard length > 0, String(data: data.prefix(length), encoding: .utf8) != nil else {
                throw FilesFailure.invalidText
            }
            pages.append(offset..<(offset + length)); offset += length
        }
        return Self(pages: pages, byteCount: Int(size))
    }

    func page(containing offset: Int) -> Int {
        pages.firstIndex { $0.contains(offset) } ?? max(0, pages.count - 1)
    }
}

actor FileTextReader {
    let url: URL
    let index: FileTextIndex
    private var cached: [Int: String] = [:]
    private var order: [Int] = []
    init(url: URL, index: FileTextIndex) { self.url = url; self.index = index }

    func page(_ number: Int) throws -> String {
        try Task.checkCancellation()
        guard index.pages.indices.contains(number) else { return "" }
        if let text = cached[number] { return text }
        let range = index.pages[number]
        let file = try FileHandle(forReadingFrom: url)
        defer { try? file.close() }
        try file.seek(toOffset: UInt64(range.lowerBound))
        let data = try file.read(upToCount: range.count) ?? Data()
        guard data.count == range.count, let text = String(data: data, encoding: .utf8) else {
            throw FilesFailure.invalidText
        }
        if order.count >= 8 { cached.removeValue(forKey: order.removeFirst()) }
        cached[number] = text; order.append(number)
        return text
    }

    /// A split literal needs the next page's prefix, not an unbounded document.
    func content(_ number: Int, searchQuery: String? = nil) throws -> FileTextPageContent {
        let text = try page(number)
        guard let query = searchQuery, !query.isEmpty, query.utf8.count <= 512,
              text.range(of: query, options: .caseInsensitive) == nil,
              index.pages.indices.contains(number + 1) else {
            return .init(text: text, includesNextPage: false)
        }
        let next = String(try page(number + 1).prefix(512))
        guard (String(text.suffix(512)) + next).range(of: query, options: .caseInsensitive) != nil else {
            return .init(text: text, includesNextPage: false)
        }
        return .init(text: text + next, includesNextPage: true)
    }

    /// Search does not promote contents to Markdown or keep unbounded match/result arrays.
    func matches(_ query: String) throws -> [Int] {
        guard !query.isEmpty, query.utf8.count <= 512 else { return [] }
        // Include adjacent chunks so a literal match spanning the split remains reachable.
        var result: [Int] = [], previous = ""
        for number in index.pages.indices {
            try Task.checkCancellation()
            let text = try page(number)
            if (previous + text).range(of: query, options: .caseInsensitive) != nil {
                result.append(max(0, number - (text.range(of: query, options: .caseInsensitive) == nil ? 1 : 0)))
            }
            previous = String(text.suffix(512))
            if result.count == 100 { break }
        }
        return Array(Set(result)).sorted()
    }
}

struct FileTextPageContent: Sendable {
    let text: String
    let includesNextPage: Bool
}

enum FileCodePresentation {
    static func language(name: String) -> String? {
        let map = ["swift": "swift", "rs": "rust", "kt": "kotlin", "py": "python", "js": "javascript",
                   "ts": "typescript", "json": "json", "yaml": "yaml", "yml": "yaml", "sh": "bash",
                   "html": "html", "css": "css", "xml": "xml", "toml": "toml", "java": "java",
                   "c": "c", "h": "c", "cpp": "cpp", "sql": "sql"]
        return map[(name as NSString).pathExtension.lowercased()]
    }
    static func markdownSource(_ text: String, language: String) -> String {
        var longest = 0, current = 0
        for character in text {
            if character == "`" { current += 1; longest = max(longest, current) } else { current = 0 }
        }
        let fence = String(repeating: "`", count: max(3, longest + 1))
        return fence + language + "\n" + text + "\n" + fence
    }
}
