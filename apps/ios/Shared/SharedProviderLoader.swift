import Foundation
import UniformTypeIdentifiers
import UIKit

enum SharedProviderItem: Sendable {
    case text(String), url(URL), file(SharedImportFile)
}

/// One provider may advertise both text and a file URL. Keep files as attachments.
@MainActor
enum SharedProviderLoader {
    static func load(_ provider: NSItemProvider) async throws -> SharedProviderItem {
        try Task.checkCancellation()
        let fileURL = provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier)
        let namedFile = provider.suggestedName.map { !($0 as NSString).pathExtension.isEmpty } == true
        if fileURL || (namedFile && !provider.hasItemConformingToTypeIdentifier(UTType.url.identifier)) {
            if let type = fileType(provider, preferCompatibleImage: false) { return .file(try await file(provider, type: type)) }
            if fileURL { return .file(try await fileURLItem(provider)) }
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
            let item = try await item(provider, type: UTType.url.identifier)
            if case .url(let url) = item, ["http", "https"].contains(url.scheme?.lowercased() ?? "") { return item }
            throw SharedImportError.unreadableFile
        }
        if let textType = provider.registeredTypeIdentifiers.first(where: { UTType($0)?.conforms(to: .plainText) == true || $0 == UTType.text.identifier }) {
            let value = try await item(provider, type: textType)
            guard case .text(let text) = value, text.utf8.count <= SharedImportStore.maxTextBytes else {
                throw SharedImportError.limitExceeded
            }
            return value
        }
        if fileType(provider) == nil,
           provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) ||
            provider.registeredTypeIdentifiers.contains("com.apple.uikit.image") {
            return .file(try await imageObject(provider))
        }
        guard let type = fileType(provider) else { throw SharedImportError.unreadableFile }
        return .file(try await file(provider, type: type))
    }

    private static func fileType(_ provider: NSItemProvider, preferCompatibleImage: Bool = true) -> String? {
        let types = provider.registeredTypeIdentifiers.filter {
            guard let type = UTType($0) else { return false }
            // UIKit's object archive is not an encoded image attachment.
            return $0 != "com.apple.uikit.image" && type.conforms(to: .data) && !type.conforms(to: .url)
                && (!type.conforms(to: .image) || type.preferredMIMEType != nil)
        }
        if preferCompatibleImage, let compatible = types.first(where: {
            ["image/png", "image/jpeg", "image/gif", "image/webp"].contains(UTType($0)?.preferredMIMEType ?? "")
        }) { return compatible }
        return types.first
    }

    private static func imageObject(_ provider: NSItemProvider) async throws -> SharedImportFile {
        let image: UIImage
        do {
            guard provider.canLoadObject(ofClass: UIImage.self) else { throw SharedImportError.unreadableFile }
            image = try await withCheckedThrowingContinuation { continuation in
                provider.loadObject(ofClass: UIImage.self) { value, error in
                    guard let image = value as? UIImage, error == nil else {
                        continuation.resume(throwing: SharedImportError.unreadableFile); return
                    }
                    continuation.resume(returning: image)
                }
            }
        } catch {
            try Task.checkCancellation()
            let type = provider.hasItemConformingToTypeIdentifier(UTType.image.identifier)
                ? UTType.image.identifier : "com.apple.uikit.image"
            image = try await withCheckedThrowingContinuation { continuation in
                provider.loadItem(forTypeIdentifier: type, options: nil) { value, error in
                    guard let image = value as? UIImage, error == nil else {
                        continuation.resume(throwing: SharedImportError.unreadableFile); return
                    }
                    continuation.resume(returning: image)
                }
            }
        }
        try Task.checkCancellation()
        // Bound additional PNG encoding allocations for object-only providers.
        let pixels = image.size.width * image.scale * image.size.height * image.scale
        guard pixels.isFinite, pixels > 0, pixels <= CGFloat(SharedImportStore.maxBytes / 4),
              let data = image.pngData(), data.count <= SharedImportStore.maxBytes else {
            throw SharedImportError.limitExceeded
        }
        return SharedImportFile(name: filename(provider.suggestedName ?? "image", type: UTType.png.identifier),
                                typeIdentifier: UTType.png.identifier, data: data)
    }

    private static func file(_ provider: NSItemProvider, type: String) async throws -> SharedImportFile {
        let name = provider.suggestedName
        do {
            return try await withCheckedThrowingContinuation { continuation in
                provider.loadFileRepresentation(forTypeIdentifier: type) { url, error in
                    do {
                        guard let url, error == nil else { throw SharedImportError.unreadableFile }
                        // The temporary provider URL expires when this callback returns.
                        continuation.resume(returning: try SharedImportStore.readFile(url,
                            name: filename(name ?? url.lastPathComponent, type: type), typeIdentifier: type))
                    } catch { continuation.resume(throwing: error) }
                }
            }
        } catch {
            try Task.checkCancellation()
            if error as? SharedImportError == .limitExceeded { throw error }
            // Photos and data-only providers need not offer a temporary-file representation.
            let data: Data = try await withCheckedThrowingContinuation { continuation in
                provider.loadDataRepresentation(forTypeIdentifier: type) { data, error in
                    guard let data, error == nil else {
                        continuation.resume(throwing: SharedImportError.unreadableFile); return
                    }
                    guard data.count <= SharedImportStore.maxBytes else {
                        continuation.resume(throwing: SharedImportError.limitExceeded); return
                    }
                    continuation.resume(returning: data)
                }
            }
            try Task.checkCancellation()
            return SharedImportFile(name: filename(name ?? "attachment", type: type), typeIdentifier: type, data: data)
        }
    }

    private static func fileURLItem(_ provider: NSItemProvider) async throws -> SharedImportFile {
        let name = provider.suggestedName
        return try await withCheckedThrowingContinuation { continuation in
            provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, error in
                do {
                    guard let url = item as? URL, url.isFileURL, error == nil else { throw SharedImportError.unreadableFile }
                    let type = UTType(filenameExtension: url.pathExtension)?.identifier ?? UTType.data.identifier
                    continuation.resume(returning: try SharedImportStore.readFile(url, name: name ?? url.lastPathComponent, typeIdentifier: type))
                } catch { continuation.resume(throwing: error) }
            }
        }
    }

    private static func item(_ provider: NSItemProvider, type: String) async throws -> SharedProviderItem {
        if type == UTType.url.identifier, provider.canLoadObject(ofClass: NSURL.self) {
            do {
                return try await withCheckedThrowingContinuation { continuation in
                    provider.loadObject(ofClass: NSURL.self) { value, error in
                        guard let value = value as? URL, error == nil else {
                            continuation.resume(throwing: SharedImportError.unreadableFile); return
                        }
                        continuation.resume(returning: .url(value))
                    }
                }
            } catch {
                try Task.checkCancellation()
            }
        }
        if UTType(type)?.conforms(to: .text) == true, provider.canLoadObject(ofClass: NSString.self) {
            do {
                return try await withCheckedThrowingContinuation { continuation in
                    provider.loadObject(ofClass: NSString.self) { value, error in
                        guard let text = value as? String, error == nil else {
                            continuation.resume(throwing: SharedImportError.unreadableFile); return
                        }
                        continuation.resume(returning: .text(text))
                    }
                }
            } catch {
                // A remote provider can advertise NSString but reject that reader.
                // Fall back to its actual legacy representation, not an empty preview.
                try Task.checkCancellation()
            }
        }
        return try await withCheckedThrowingContinuation { continuation in
            provider.loadItem(forTypeIdentifier: type, options: nil) { item, error in
                if error != nil { continuation.resume(throwing: SharedImportError.unreadableFile) }
                else if let url = item as? URL { continuation.resume(returning: .url(url)) }
                else if let text = item as? String { continuation.resume(returning: .text(text)) }
                else if let text = item as? NSAttributedString { continuation.resume(returning: .text(text.string)) }
                else if let data = item as? Data, data.count <= SharedImportStore.maxTextBytes,
                        let text = String(data: data, encoding: .utf8) {
                    if type == UTType.url.identifier, let url = URL(string: text) { continuation.resume(returning: .url(url)) }
                    else { continuation.resume(returning: .text(text)) }
                }
                else { continuation.resume(throwing: SharedImportError.unreadableFile) }
            }
        }
    }

    private nonisolated static func filename(_ name: String, type: String) -> String {
        let safe = SharedImportStore.safeName(name)
        guard (safe as NSString).pathExtension.isEmpty, let suffix = UTType(type)?.preferredFilenameExtension else { return safe }
        return SharedImportStore.safeName(safe + "." + suffix)
    }
}
