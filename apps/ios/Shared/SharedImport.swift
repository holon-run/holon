import Foundation
import Darwin

struct SharedImportAttachment: Codable, Equatable, Sendable {
    let id: UUID
    let name: String
    let typeIdentifier: String
    let byteCount: Int
}
struct SharedImportPayload: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let createdAt: Date
    let text: String
    let urls: [URL]
    let attachments: [SharedImportAttachment]
}
struct SharedImportFile: Sendable {
    let name: String
    let typeIdentifier: String
    let data: Data
}
enum SharedImportError: Error, Equatable {
    case unavailableAppGroup, limitExceeded, unreadableFile, invalidRecord, storageUnavailable
}

/// No credentials, destinations or external file paths are persisted here.
final class SharedImportStore: @unchecked Sendable {
    static let maxRecords = 20
    static let maxBytes = 20 * 1024 * 1024
    static let maxItems = 20
    static let maxAttachments = 10
    static let maxTextBytes = 64 * 1024

    static func sendingText(text: String, urls: [URL]) -> String {
        ([text] + urls.map(\.absoluteString)).filter { !$0.isEmpty }.joined(separator: "\n\n")
    }

    static func validate(text: String, urls: [URL], attachmentBytes: [Int]) throws {
        guard urls.count + attachmentBytes.count <= maxItems,
              attachmentBytes.count <= maxAttachments,
              urls.allSatisfy({ ["http", "https"].contains($0.scheme?.lowercased() ?? "") }),
              sendingText(text: text, urls: urls).utf8.count <= maxTextBytes,
              attachmentBytes.allSatisfy({ $0 >= 0 && $0 <= maxBytes }),
              attachmentBytes.reduce(0, +) <= maxBytes,
              !text.isEmpty || !urls.isEmpty || !attachmentBytes.isEmpty else {
            throw SharedImportError.limitExceeded
        }
    }
    private let root: URL
    private let manager = FileManager.default

    static func configured(bundle: Bundle = .main) throws -> SharedImportStore {
        guard let group = bundle.object(forInfoDictionaryKey: "HolonSharedAppGroup") as? String,
              !group.isEmpty, !group.contains("$("),
              let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
        else { throw SharedImportError.unavailableAppGroup }
        return try SharedImportStore(container: container)
    }

    /// Explicit container injection is for hosted tests; production must use configured().
    init(container: URL) throws {
        root = container.appendingPathComponent("SharedImports", isDirectory: true)
        try manager.createDirectory(at: root, withIntermediateDirectories: true,
                                    attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
        guard try root.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink != true else {
            throw SharedImportError.invalidRecord
        }
    }

    private func locked<T>(_ body: () throws -> T) throws -> T {
        let fd = open(root.appendingPathComponent(".lock").path, O_CREAT | O_RDWR | O_NOFOLLOW, S_IRUSR | S_IWUSR)
        guard fd >= 0 else { throw SharedImportError.storageUnavailable }
        defer { close(fd) }
        guard flock(fd, LOCK_EX) == 0 else { throw SharedImportError.storageUnavailable }
        defer { flock(fd, LOCK_UN) }
        // An interrupted writer is never a visible record.
        for url in try manager.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)
        where url.lastPathComponent.hasPrefix(".pending-") { try manager.removeItem(at: url) }
        return try body()
    }

    static func readFile(_ url: URL, name: String, typeIdentifier: String) throws -> SharedImportFile {
        guard url.isFileURL else { throw SharedImportError.unreadableFile }
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        do {
            let values = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
            guard values.isRegularFile == true else { throw SharedImportError.unreadableFile }
            guard (values.fileSize ?? Int.max) <= maxBytes else { throw SharedImportError.limitExceeded }
            let handle = try FileHandle(forReadingFrom: url)
            defer { try? handle.close() }
            let data = try handle.read(upToCount: maxBytes + 1) ?? Data()
            guard data.count <= maxBytes else { throw SharedImportError.limitExceeded }
            return SharedImportFile(name: name, typeIdentifier: typeIdentifier, data: data)
        } catch let error as SharedImportError { throw error }
        catch { throw SharedImportError.unreadableFile }
    }

    @discardableResult
    func stage(text: String, urls: [URL], files: [SharedImportFile]) throws -> SharedImportPayload {
        try Self.validate(text: text, urls: urls, attachmentBytes: files.map { $0.data.count })
        return try locked {
            guard try loadUnlocked().count < Self.maxRecords else { throw SharedImportError.limitExceeded }
            let id = UUID()
            let temporary = root.appendingPathComponent(".pending-" + id.uuidString, isDirectory: true)
            try manager.createDirectory(at: temporary, withIntermediateDirectories: false,
                                        attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
            var committed = false
            defer { if !committed { try? manager.removeItem(at: temporary) } }
            let attachments = try files.map { file in
                let attachment = SharedImportAttachment(id: UUID(), name: Self.safeName(file.name),
                    typeIdentifier: String(file.typeIdentifier.prefix(128)), byteCount: file.data.count)
                try file.data.write(to: temporary.appendingPathComponent(attachment.id.uuidString), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
                return attachment
            }
            let payload = SharedImportPayload(id: id, createdAt: Date(), text: text, urls: urls, attachments: attachments)
            try JSONEncoder().encode(payload).write(to: temporary.appendingPathComponent("record.json"), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            try manager.moveItem(at: temporary, to: root.appendingPathComponent(id.uuidString))
            committed = true
            return payload
        }
    }
    static func safeName(_ name: String) -> String {
        // Preserve the joiner in compound emoji, but strip other controls and path separators.
        let cleaned = name.unicodeScalars.filter {
            ($0.value == 0x200D || !CharacterSet.controlCharacters.contains($0)) &&
                !"/\\:".unicodeScalars.contains($0)
        }
        let result = String(String.UnicodeScalarView(cleaned)).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !result.isEmpty, result != ".", result != ".." else { return "attachment" }
        let maxBytes = 255 // Matches the host's import metadata limit.
        guard result.utf8.count > maxBytes else { return result }
        var stem = result[...]
        var suffix = ""
        if let dot = result.lastIndex(of: "."), dot != result.startIndex {
            let candidate = result[dot...]
            if candidate.count > 1, candidate.utf8.count <= 33 {
                suffix = String(candidate)
                stem = result[..<dot]
            }
        }
        var prefix = ""
        var bytes = 0
        for character in stem {
            let count = String(character).utf8.count
            guard bytes + count <= maxBytes - suffix.utf8.count else { break }
            prefix.append(character)
            bytes += count
        }
        if prefix.isEmpty || prefix == "." || prefix == ".." { prefix = "attachment" }
        return prefix + suffix
    }
    func load() throws -> [SharedImportPayload] { try locked { try loadUnlocked() } }
    private func loadUnlocked() throws -> [SharedImportPayload] {
        try manager.contentsOfDirectory(at: root, includingPropertiesForKeys: nil).compactMap { directory in
            guard let id = UUID(uuidString: directory.lastPathComponent) else { return nil }
            guard try directory.resourceValues(forKeys: [.isSymbolicLinkKey, .isDirectoryKey]).isSymbolicLink != true,
                  try directory.resourceValues(forKeys: [.isDirectoryKey]).isDirectory == true else {
                throw SharedImportError.invalidRecord
            }
            let record = directory.appendingPathComponent("record.json")
            guard try record.resourceValues(forKeys: [.isSymbolicLinkKey, .fileSizeKey]).isSymbolicLink != true,
                  (try record.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? Int.max) <= Self.maxBytes else { throw SharedImportError.invalidRecord }
            let payload = try JSONDecoder().decode(SharedImportPayload.self, from: Data(contentsOf: record))
            guard payload.id == id, payload.attachments.count + payload.urls.count <= Self.maxItems else { throw SharedImportError.invalidRecord }
            do {
                try Self.validate(text: payload.text, urls: payload.urls, attachmentBytes: payload.attachments.map(\.byteCount))
            } catch { throw SharedImportError.invalidRecord }
            return payload
        }.sorted { $0.createdAt < $1.createdAt }
    }
    func attachmentData(payloadID: UUID, attachmentID: UUID) throws -> Data {
        try locked {
            guard let payload = try loadUnlocked().first(where: { $0.id == payloadID }),
                  let attachment = payload.attachments.first(where: { $0.id == attachmentID }) else { throw SharedImportError.invalidRecord }
            let url = root.appendingPathComponent(payloadID.uuidString).appendingPathComponent(attachmentID.uuidString)
            guard try url.resourceValues(forKeys: [.isSymbolicLinkKey, .isRegularFileKey]).isSymbolicLink != true else { throw SharedImportError.invalidRecord }
            let data = try Self.readFile(url, name: attachment.name, typeIdentifier: attachment.typeIdentifier).data
            guard data.count == attachment.byteCount else { throw SharedImportError.invalidRecord }
            return data
        }
    }
    /// Call only after durable outbox enqueue succeeded. Failure leaves the record recoverable.
    func consume(id: UUID, enqueueSucceeded: Bool) throws {
        guard enqueueSucceeded else { return }
        try discard(id: id)
    }
    func discard(id: UUID) throws {
        try locked { try manager.removeItem(at: root.appendingPathComponent(id.uuidString)) }
    }
}
