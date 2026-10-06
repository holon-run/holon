import UIKit
import UniformTypeIdentifiers
import ImageIO

@MainActor
final class ShareViewController: UIViewController {
    private enum LoadedItem: Sendable { case text(String), url(URL) }
    private var store: SharedImportStore?
    private var text = ""
    private var urls: [URL] = []
    private var files: [SharedImportFile] = []
    private let preview = UITextView()
    private let imagePreview = UIImageView()
    private let save = UIButton(type: .system)
    private var cancelled = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        preview.isEditable = false
        save.setTitle(NSLocalizedString("share.save", comment: ""), for: .normal)
        save.isEnabled = false
        save.addTarget(self, action: #selector(confirm), for: .touchUpInside)
        let cancel = UIButton(type: .system)
        cancel.setTitle(NSLocalizedString("share.cancel", comment: ""), for: .normal)
        cancel.addTarget(self, action: #selector(cancelShare), for: .touchUpInside)
        imagePreview.contentMode = .scaleAspectFit
        imagePreview.isHidden = true
        imagePreview.heightAnchor.constraint(equalToConstant: 160).isActive = true
        let stack = UIStackView(arrangedSubviews: [preview, imagePreview, save, cancel])
        stack.axis = .vertical
        stack.spacing = 16
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 16),
            stack.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -16),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -16)
        ])
        do { store = try SharedImportStore.configured() }
        catch { preview.text = NSLocalizedString("share.unavailable", comment: ""); return }
        Task { await loadPreview() }
    }
    private func loadPreview() async {
        let providers = (extensionContext?.inputItems as? [NSExtensionItem] ?? []).flatMap { $0.attachments ?? [] }
        guard !providers.isEmpty, providers.count <= SharedImportStore.maxItems else { fail(); return }
        do {
            for provider in providers {
                guard !cancelled else { return }
                if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
                   !provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
                    let item = try await loadItem(provider, type: UTType.url.identifier)
                    guard case .url(let url) = item, ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { throw SharedImportError.unreadableFile }
                    urls.append(url)
                } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
                    let item = try await loadItem(provider, type: UTType.plainText.identifier)
                    guard case .text(let value) = item else { throw SharedImportError.unreadableFile }
                    text += (text.isEmpty ? "" : "\n") + value
                } else {
                    guard let type = provider.registeredTypeIdentifiers.first(where: {
                        guard let type = UTType($0) else { return false }
                        return type.conforms(to: .data)
                    }) else { throw SharedImportError.unreadableFile }
                    let file: SharedImportFile = try await withCheckedThrowingContinuation { continuation in
                        provider.loadFileRepresentation(forTypeIdentifier: type) { url, error in
                            do {
                                guard let url, error == nil else { throw SharedImportError.unreadableFile }
                                // Copy inside callback: the provider URL expires when it returns.
                                continuation.resume(returning: try SharedImportStore.readFile(url, name: url.lastPathComponent, typeIdentifier: type))
                            } catch { continuation.resume(throwing: error) }
                        }
                    }
                    files.append(file)
                    if imagePreview.image == nil, UTType(file.typeIdentifier)?.conforms(to: .image) == true {
                        // Decode a bounded thumbnail, never the full-resolution provider image.
                        if let source = CGImageSourceCreateWithData(file.data as CFData, nil),
                           let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                            kCGImageSourceCreateThumbnailFromImageAlways: true,
                            kCGImageSourceThumbnailMaxPixelSize: 512
                           ] as CFDictionary) {
                            imagePreview.image = UIImage(cgImage: image)
                            imagePreview.isHidden = false
                        }
                    }
                }
                try SharedImportStore.validate(text: text, urls: urls, attachmentBytes: files.map { $0.data.count })
            }
            guard !cancelled else { return }
            preview.text = ([NSLocalizedString("share.stageOnly", comment: ""), text] + urls.map(\.absoluteString) + files.map { "\(SharedImportStore.safeName($0.name)) (\($0.data.count) bytes)" }).filter { !$0.isEmpty }.joined(separator: "\n\n")
            save.isEnabled = true
        } catch { fail() }
    }
    private func loadItem(_ provider: NSItemProvider, type: String) async throws -> LoadedItem {
        try await withCheckedThrowingContinuation { continuation in
            provider.loadItem(forTypeIdentifier: type, options: nil) { item, error in
                if let error { continuation.resume(throwing: error) }
                else if let url = item as? URL { continuation.resume(returning: .url(url)) }
                else if let text = item as? String { continuation.resume(returning: .text(text)) }
                else { continuation.resume(throwing: SharedImportError.unreadableFile) }
            }
        }
    }
    private func fail() {
        imagePreview.image = nil
        imagePreview.isHidden = true
        files = []; urls = []; text = ""
        save.isEnabled = false
        preview.text = NSLocalizedString("share.invalid", comment: "")
    }
    @objc private func confirm() {
        guard !cancelled, let store else { return }
        save.isEnabled = false
        do {
            try store.stage(text: text, urls: urls, files: files)
            extensionContext?.completeRequest(returningItems: nil)
        } catch {
            preview.text = NSLocalizedString("share.saveFailed", comment: "")
            save.isEnabled = true
        }
    }
    @objc private func cancelShare() {
        cancelled = true
        files = []; urls = []; text = ""
        extensionContext?.cancelRequest(withError: NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
    }
}
