import UIKit
import UniformTypeIdentifiers
import HolonClient
import ImageIO

@MainActor
final class ShareViewController: UIViewController, UITableViewDataSource, UITableViewDelegate, UISearchBarDelegate {
    private var store: SharedImportStore?
    private var sender: SharedAgentSender?
    private var text = ""
    private var urls: [URL] = []
    private var files: [SharedImportFile] = []
    private var agents: [SharedAgent] = []
    private var selected: SharedAgent?
    private var staged: SharedImportPayload?
    private var ready = false
    private var connectionDescription = ""
    private var busy = false
    private var cancelled = false
    private var loading: Task<Void, Never>?
    private var sending: Task<Void, Never>?
    private let preview = UITextView()
    private let imagePreview = UIImageView()
    private let status = UILabel()
    private let search = UISearchBar()
    private let recipients = UITableView(frame: .zero, style: .plain)
    private let send = UIButton(type: .system)
    private let save = UIButton(type: .system)
    private var filtered: [SharedAgent] {
        let query = search.text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return query.isEmpty ? agents : agents.filter { $0.name.localizedCaseInsensitiveContains(query) || $0.id.localizedCaseInsensitiveContains(query) }
    }
    private func localized(_ key: String) -> String { NSLocalizedString(key, comment: "") }
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        let title = UILabel()
        title.text = localized("share.title")
        title.font = .preferredFont(forTextStyle: .headline)
        title.adjustsFontForContentSizeCategory = true
        status.font = .preferredFont(forTextStyle: .caption1)
        status.textColor = .secondaryLabel
        status.numberOfLines = 0
        status.adjustsFontForContentSizeCategory = true
        status.accessibilityIdentifier = "share.status"
        preview.isEditable = false
        preview.font = .preferredFont(forTextStyle: .body)
        preview.adjustsFontForContentSizeCategory = true
        preview.backgroundColor = .secondarySystemBackground
        preview.layer.cornerRadius = 12
        preview.accessibilityIdentifier = "share.extensionPreview"
        preview.heightAnchor.constraint(equalToConstant: 120).isActive = true
        imagePreview.contentMode = .scaleAspectFit
        imagePreview.isHidden = true
        imagePreview.heightAnchor.constraint(equalToConstant: 100).isActive = true
        imagePreview.accessibilityIdentifier = "share.imagePreview"
        imagePreview.isAccessibilityElement = true
        imagePreview.accessibilityTraits = .image
        imagePreview.accessibilityLabel = localized("share.imagePreview")
        search.placeholder = localized("share.search")
        search.searchTextField.accessibilityIdentifier = "share.search"
        search.delegate = self
        search.searchBarStyle = .minimal
        recipients.dataSource = self; recipients.delegate = self
        recipients.keyboardDismissMode = .onDrag
        recipients.accessibilityIdentifier = "share.agents"
        send.setTitle(localized("share.send"), for: .normal)
        send.configuration = .filled()
        send.isEnabled = false
        send.accessibilityIdentifier = "share.send"
        send.addTarget(self, action: #selector(confirmSend), for: .touchUpInside)
        save.setTitle(localized("share.save"), for: .normal)
        save.isEnabled = false
        save.accessibilityIdentifier = "share.stage"
        save.addTarget(self, action: #selector(saveForLater), for: .touchUpInside)
        let cancel = UIButton(type: .system)
        cancel.setTitle(localized("share.cancel"), for: .normal)
        cancel.addTarget(self, action: #selector(cancelShare), for: .touchUpInside)
        let footer = UIStackView(arrangedSubviews: [save, cancel])
        footer.distribution = .fillEqually
        let stack = UIStackView(arrangedSubviews: [title, status, preview, imagePreview, search, recipients, send, footer])
        stack.axis = .vertical; stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 16),
            stack.bottomAnchor.constraint(equalTo: view.keyboardLayoutGuide.topAnchor, constant: -12),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -16),
            send.heightAnchor.constraint(greaterThanOrEqualToConstant: 44),
            footer.heightAnchor.constraint(greaterThanOrEqualToConstant: 44)
        ])
        do { store = try .configured() }
        catch { status.text = localized("share.unavailable"); return }
        status.text = localized("share.loading")
    }
    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        // Wait until the remote controller is attached before reading its providers.
        guard loading == nil, store != nil else { return }
        loading = Task {
            await loadPreview()
            guard ready, !cancelled, !Task.isCancelled else { return }
            await loadAgents()
        }
    }
    private func loadPreview() async {
        let items = extensionContext?.inputItems as? [NSExtensionItem] ?? []
        let providers = items.flatMap { $0.attachments ?? [] }
        // UIActivityViewController can supply text on the item itself, without a provider.
        text = items.compactMap { $0.attributedContentText?.string }.filter { !$0.isEmpty }.joined(separator: "\n")
        guard !text.isEmpty || !providers.isEmpty, items.count <= SharedImportStore.maxItems,
              providers.count <= SharedImportStore.maxItems else { failPreview(); return }
        do {
            for provider in providers {
                let item = try await SharedProviderLoader.load(provider)
                guard !cancelled, !Task.isCancelled else { return }
                switch item {
                case .url(let url): urls.append(url)
                case .text(let value):
                    if text != value { text += (text.isEmpty ? "" : "\n") + value }
                case .file(let file):
                    files.append(file)
                    if imagePreview.image == nil, UTType(file.typeIdentifier)?.conforms(to: .image) == true,
                       let source = CGImageSourceCreateWithData(file.data as CFData, nil),
                       let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                        kCGImageSourceCreateThumbnailFromImageAlways: true,
                        kCGImageSourceThumbnailMaxPixelSize: 512
                       ] as CFDictionary) {
                        imagePreview.image = UIImage(cgImage: image)
                        imagePreview.isHidden = false
                    }
                }
                try SharedImportStore.validate(text: text, urls: urls, attachmentBytes: files.map { $0.data.count })
            }
            try SharedImportStore.validate(text: text, urls: urls, attachmentBytes: files.map { $0.data.count })
            preview.text = ([text] + urls.map(\.absoluteString) + files.map {
                "\(SharedImportStore.safeName($0.name)) · \(ByteCountFormatter.string(fromByteCount: Int64($0.data.count), countStyle: .file))"
            }).filter { !$0.isEmpty }.joined(separator: "\n\n")
            ready = true; save.isEnabled = true
        } catch {
            if !cancelled, !Task.isCancelled { failPreview() }
        }
    }
    private func loadAgents() async {
        do {
            let vault = try SharedSessionVault.configured()
            guard let session = try vault.read() else { throw SharedShareError.loginRequired }
            let sender = try SharedAgentSender(session: session, vault: vault)
            self.sender = sender
            let agents = try await sender.agents()
            guard !cancelled, !Task.isCancelled else { await sender.close(); return }
            self.agents = agents
            let endpoint = session.apiBaseURL.absoluteString
            connectionDescription = session.connectionName == session.apiBaseURL.host
                ? endpoint : "\(session.connectionName) · \(endpoint)"
            status.text = connectionDescription
            if agents.isEmpty { status.text = localized("share.noAgents") }
            recipients.reloadData()
        } catch { if !cancelled { status.text = message(for: error) } }
    }
    private func failPreview() {
        imagePreview.image = nil; imagePreview.isHidden = true
        files = []; urls = []; text = ""; ready = false
        save.isEnabled = false; send.isEnabled = false
        status.text = localized("share.invalid")
    }
    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { filtered.count }
    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let agent = filtered[indexPath.row]
        let cell = UITableViewCell(style: .subtitle, reuseIdentifier: nil)
        cell.textLabel?.text = agent.name
        cell.detailTextLabel?.text = agent.id == agent.name ? nil : agent.id
        cell.imageView?.image = UIImage(systemName: "person.crop.circle")
        cell.imageView?.tintColor = .systemBlue
        cell.accessoryType = selected?.id == agent.id ? .checkmark : .none
        cell.accessibilityIdentifier = "share.agent." + agent.id
        return cell
    }
    func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        guard !busy, staged?.delivery == nil else { return }
        selected = filtered[indexPath.row]
        view.endEditing(true)
        recipients.reloadData()
        send.isEnabled = ready
        send.setTitle(String(format: localized("share.sendTo"), selected!.name), for: .normal)
    }
    func searchBar(_ searchBar: UISearchBar, textDidChange searchText: String) { recipients.reloadData() }
    @objc private func confirmSend() {
        guard ready, !busy, !cancelled, let selected else { return }
        let alert = UIAlertController(title: String(format: localized("share.sendTo"), selected.name),
            message: connectionDescription + "\n\n" + preview.text, preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: localized("share.cancel"), style: .cancel))
        alert.addAction(UIAlertAction(title: localized("share.send"), style: .default) { [weak self] _ in
            self?.startSending()
        })
        present(alert, animated: true)
    }
    private func stage() throws -> SharedImportPayload {
        if let staged { return staged }
        guard let store else { throw SharedImportError.unavailableAppGroup }
        let payload = try store.stage(text: text, urls: urls, files: files)
        staged = payload; files = []
        return payload
    }
    private func startSending() {
        guard !busy, !cancelled, let sender, let selected, let store else { return }
        busy = true; send.isEnabled = false; save.isEnabled = false
        recipients.isUserInteractionEnabled = false; search.isUserInteractionEnabled = false
        status.text = localized("share.sending")
        sending = Task {
            do {
                let payload = try stage()
                try await sender.send(payloadID: payload.id, agentID: selected.id, store: store)
                guard !cancelled else { return }
                status.text = localized("share.sent")
                send.setTitle(localized("share.done"), for: .normal)
                send.removeTarget(self, action: #selector(confirmSend), for: .touchUpInside)
                send.addTarget(self, action: #selector(finishShare), for: .touchUpInside)
                send.isEnabled = true
            } catch {
                guard !cancelled else { return }
                if let id = staged?.id, let restored = try? store.load().first(where: { $0.id == id }) { staged = restored }
                let unknown = staged?.delivery?.state == .unknown
                status.text = unknown ? localized("share.unknown") + "\n" + message(for: error) : message(for: error)
                send.setTitle(localized(unknown ? "share.retrySame" : "share.retry"), for: .normal)
                send.isEnabled = true; save.isEnabled = true
                recipients.isUserInteractionEnabled = staged?.delivery == nil
                search.isUserInteractionEnabled = staged?.delivery == nil
            }
            busy = false
        }
    }
    private func message(for error: Error) -> String {
        if let failure = error as? HolonHTTPFailure {
            if failure.statusCode == 401 { return localized("share.loginRequired") }
            if failure.statusCode == 403 { return localized("share.denied") }
            if failure.statusCode == 409 { return localized("share.conflict") }
        }
        if let failure = error as? SharedShareError {
            return localized(failure == .loginRequired ? "share.loginRequired" : "share.reconnect")
        }
        return localized("share.offline")
    }
    @objc private func saveForLater() {
        guard !cancelled, !busy else { return }
        do { _ = try stage(); finishShare() }
        catch { status.text = localized("share.saveFailed") }
    }
    @objc private func finishShare() {
        if let sender { Task { await sender.close() } }
        extensionContext?.completeRequest(returningItems: nil)
    }
    @objc private func cancelShare() {
        cancelled = true
        loading?.cancel(); sending?.cancel()
        if let sender { Task { await sender.close() } }
        files = []; urls = []; text = ""
        extensionContext?.cancelRequest(withError: NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
    }
}
