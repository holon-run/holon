import Foundation
import CryptoKit
import HolonClient

struct FilesWorkspace: Identifiable, Equatable, Sendable {
    let workspaceID: String
    let executionRootID: String?
    let name: String
    var id: String { workspaceID + "|" + (executionRootID ?? "") }
}

struct FilesEntry: Identifiable, Equatable, Sendable {
    let name: String
    let path: String
    let isDirectory: Bool
    var id: String { path }
}

struct FilesDirectory: Equatable, Sendable {
    let workspace: FilesWorkspace
    let path: String
    let entries: [FilesEntry]

    func filtered(query: String, showHidden: Bool) -> [FilesEntry] {
        entries.filter {
            (showHidden || !$0.name.hasPrefix(".")) &&
                (query.isEmpty || $0.name.localizedCaseInsensitiveContains(query))
        }.sorted {
            if $0.isDirectory != $1.isDirectory { return $0.isDirectory }
            return $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }
}

enum FilesSource: Equatable, Sendable {
    case workspace(FilesWorkspace, path: String)
    // A reference is opaque server input, never a device URL.
    case reference(String)
}

enum FilesFailure: Error, Equatable, Sendable {
    case forbidden, deleted, unsupported, tooLarge, unavailable, invalidReference

    var key: String {
        switch self {
        case .forbidden: "files.error.forbidden"
        case .deleted: "files.error.deleted"
        case .unsupported: "files.error.unsupported"
        case .tooLarge: "files.error.tooLarge"
        case .unavailable: "files.error.unavailable"
        case .invalidReference: "files.error.invalidReference"
        }
    }
}

enum FilesPreviewKind: Equatable, Sendable {
    case text, image, downloadOnly

    static func classify(mediaType: String, name: String) -> Self {
        let type = mediaType.lowercased().split(separator: ";").first.map(String.init) ?? ""
        let ext = (name as NSString).pathExtension.lowercased()
        // Never create a WebView, execute markup, or let SVG load network resources.
        if ["image/png", "image/jpeg", "image/gif", "image/webp", "image/heic"].contains(type) {
            return .image
        }
        if type.hasPrefix("text/") ||
            ["application/json", "application/xml", "application/javascript"].contains(type) ||
            ["md", "swift", "rs", "kt", "py", "js", "ts", "txt", "log", "yaml", "yml"].contains(ext) {
            return .text
        }
        return .downloadOnly
    }
}

struct FilesDownload: Sendable {
    let data: Data
    let mediaType: String
    let name: String
}

struct FilesPrepared: Identifiable, Equatable {
    let id: UUID
    let url: URL
    let name: String
    let kind: FilesPreviewKind
    let text: String?
    let truncated: Bool
}

/// No shared fallback partition; opaque directory names never include credentials.
@MainActor
final class FilesCache {
    static let maximumBytes = 16 * 1024 * 1024
    static let maximumTextBytes = 512 * 1024
    private static var activeDirectories: Set<URL> = []
    private let root: URL
    private var directory: URL?

    init(root: URL = FileManager.default.temporaryDirectory.appendingPathComponent("HolonFiles", isDirectory: true)) {
        self.root = root
    }

    func bind(_ identity: HolonConnectionIdentity) throws {
        clear()
        removeAbandonedPartitions()
        guard let runtime = identity.runtimeID, !runtime.isEmpty,
              let user = identity.userID, !user.isEmpty,
              let scope = identity.visibilityScopeID, !scope.isEmpty else {
            throw FilesFailure.forbidden
        }
        let bytes = try JSONEncoder().encode([identity.networkID, runtime, user, scope, identity.generation.uuidString])
        let key = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        let partition = root.appendingPathComponent(key, isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: partition, withIntermediateDirectories: true,
                                               attributes: [.protectionKey: FileProtectionType.complete])
        directory = partition
        Self.activeDirectories.insert(partition)
    }

    func prepare(_ download: FilesDownload) throws -> FilesPrepared {
        removeContents()
        guard download.data.count <= Self.maximumBytes else { throw FilesFailure.tooLarge }
        guard let directory else { throw FilesFailure.forbidden }
        let kind = FilesPreviewKind.classify(mediaType: download.mediaType, name: download.name)
        let name = Self.safeName(download.name)
        let url = directory.appendingPathComponent(UUID().uuidString + "-" + name)
        do {
            try download.data.write(to: url, options: [.atomic, .completeFileProtection])
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            var protected = url
            try protected.setResourceValues(values)
            let text = kind == .text ? Self.textPreview(download.data) : nil
            return FilesPrepared(id: UUID(), url: url, name: name, kind: kind,
                                 text: text, truncated: kind == .text && download.data.count > Self.maximumTextBytes)
        } catch {
            try? FileManager.default.removeItem(at: url)
            throw error
        }
    }

    private static func textPreview(_ data: Data) -> String? {
        let sample = data.prefix(maximumTextBytes)
        var boundary = sample.endIndex
        if sample.count < data.count {
            let earliest = sample.index(boundary, offsetBy: -min(3, sample.count))
            while boundary > earliest, data[boundary] & 0xC0 == 0x80 {
                boundary = data.index(before: boundary)
            }
            if boundary < sample.endIndex {
                // Validate the crossing scalar; truncation must not hide malformed UTF-8.
                var scalarEnd = sample.endIndex
                let limit = data.index(boundary, offsetBy: min(4, data.distance(from: boundary, to: data.endIndex)))
                while scalarEnd < limit, data[scalarEnd] & 0xC0 == 0x80 {
                    scalarEnd = data.index(after: scalarEnd)
                }
                guard String(data: data[boundary..<scalarEnd], encoding: .utf8)?.unicodeScalars.count == 1 else {
                    return nil
                }
            }
        }
        return String(data: sample[..<boundary], encoding: .utf8)
    }

    func owns(_ prepared: FilesPrepared) -> Bool {
        guard let directory else { return false }
        return prepared.url.deletingLastPathComponent() == directory &&
            FileManager.default.fileExists(atPath: prepared.url.path)
    }

    func removeContents() {
        guard let directory else { return }
        for url in (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? [] {
            try? FileManager.default.removeItem(at: url)
        }
    }

    func clear() {
        if let directory {
            Self.activeDirectories.remove(directory)
            try? FileManager.default.removeItem(at: directory)
            let partition = directory.deletingLastPathComponent()
            if ((try? FileManager.default.contentsOfDirectory(atPath: partition.path)) ?? []).isEmpty {
                try? FileManager.default.removeItem(at: partition)
            }
        }
        directory = nil
    }

    private func removeAbandonedPartitions() {
        let manager = FileManager.default
        for partition in (try? manager.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)) ?? [] {
            let name = partition.lastPathComponent
            guard name.count == 64, name.allSatisfy({ $0.isHexDigit }) else { continue }
            for session in (try? manager.contentsOfDirectory(at: partition, includingPropertiesForKeys: nil)) ?? [] {
                guard !Self.activeDirectories.contains(session) else { continue }
                try? manager.removeItem(at: session)
            }
            if ((try? manager.contentsOfDirectory(atPath: partition.path)) ?? []).isEmpty {
                try? manager.removeItem(at: partition)
            }
        }
    }

    static func safeName(_ name: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_."))
        let safe = String(name.unicodeScalars.map { allowed.contains($0) ? Character(String($0)) : "_" }.prefix(100))
        return safe.isEmpty || safe == "." || safe == ".." ? "artifact" : safe
    }
}
