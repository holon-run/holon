import CoreTransferable
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

enum SendingPresentation {
    static func statusKey(_ status: SendingStatus) -> String { "sending.status." + status.rawValue }
    static func stateKey(_ state: SendingState) -> String { "sending.state.\(state.rawValue)" }
    static func canRetry(_ state: SendingState) -> Bool {
        state == .queued || state == .failed || state == .unknown
    }
    static func hasContent(_ draft: SendingDraft) -> Bool {
        !draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !draft.attachments.isEmpty
    }
}

private struct SendingPhoto: Transferable {
    let url: URL
    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .image) { received in
            let values = try received.file.resourceValues(forKeys: [.fileSizeKey])
            guard let size = values.fileSize, size <= 20 * 1024 * 1024 else {
                throw SendingFailure.oversizedAttachment(received.file.lastPathComponent)
            }
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent(UUID().uuidString)
                .appendingPathExtension(received.file.pathExtension)
            try FileManager.default.copyItem(at: received.file, to: url)
            return SendingPhoto(url: url)
        }
    }
}

struct SendingView: View {
    let sender: SendingCoordinator
    var currentRunID: String? = nil
    @State private var showDraft = false
    @State private var showQueue = false
    @State private var showFiles = false
    @State private var showPhotos = false
    @State private var showStop = false
    @State private var stopRunID: String?
    @State private var photo: PhotosPickerItem?
    @State private var photoContext: SendingAttachmentImportContext?
    @State private var fileContext: SendingAttachmentImportContext?
    @State private var photoImportID: UUID?
    @State private var importError: String?

    var body: some View {
        HStack {
            Button("sending.draft") { showDraft = true }
                .accessibilityIdentifier("sending.draft")
            Button { showQueue = true } label: {
                Label("sending.queue", systemImage: "tray")
                Text("\(sender.entries.count)")
            }
            .accessibilityIdentifier("sending.queue")
            Spacer()
            Button("sending.stop", role: .destructive) {
                stopRunID = currentRunID
                showStop = stopRunID != nil
            }
                .disabled(currentRunID == nil || sender.status != .ready)
                .accessibilityIdentifier("sending.stop")
        }
        .padding()
        .background(.bar)
        .sheet(isPresented: $showDraft) { draftEditor }
        .sheet(isPresented: $showQueue) { queue }
        .confirmationDialog("sending.stop.confirm", isPresented: $showStop, titleVisibility: .visible) {
            Button("sending.stop", role: .destructive) {
                if let stopRunID { Task { await sender.stopAgent(runID: stopRunID) } }
            }
        } message: {
            Text("sending.stop.explanation")
        }
    }

    private var draftEditor: some View {
        NavigationStack {
            Form {
                Section("sending.draft") {
                    TextEditor(text: Binding(
                        get: { sender.draft.text },
                        set: { sender.editDraft(text: $0, modelID: sender.draft.modelID) }
                    ))
                    .frame(minHeight: 140)
                    .accessibilityLabel(Text("sending.text"))
                    .accessibilityIdentifier("sending.text")
                    Picker("sending.model", selection: Binding(
                        get: { sender.draft.modelID ?? "" },
                        set: { sender.editDraft(text: sender.draft.text, modelID: $0.isEmpty ? nil : $0) }
                    )) {
                        Text("sending.model.default").tag("")
                        ForEach(sender.models) { model in Text(model.name).tag(model.id) }
                        if let selected = sender.draft.modelID,
                           !sender.models.contains(where: { $0.id == selected }) {
                            Text(selected).tag(selected)
                        }
                    }
                }
                Section("sending.attachments") {
                    ForEach(sender.draft.attachments) { attachment in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(attachment.name)
                                Text(ByteCountFormatter.string(fromByteCount: attachment.byteCount, countStyle: .file))
                                    .font(.caption)
                            }
                            Spacer()
                            Button(role: .destructive) { sender.removeDraftAttachment(attachment.id) } label: {
                                Image(systemName: "trash")
                            }
                            .accessibilityLabel(Text("sending.remove"))
                        }
                    }
                    Button {
                        photoContext = sender.attachmentImportContext
                        photoImportID = nil
                        showPhotos = photoContext != nil
                    } label: { Label("sending.photo", systemImage: "photo") }
                    Button("sending.file") {
                        fileContext = sender.attachmentImportContext
                        showFiles = fileContext != nil
                    }
                    Text("sending.limits").font(.caption).foregroundStyle(.secondary)
                }
                if let error = importError ?? sender.error {
                    Section {
                        Text("sending.error.explanation")
                        Text(error).font(.caption).textSelection(.enabled)
                    }
                }
                Section {
                    Text(LocalizedStringKey(SendingPresentation.statusKey(sender.status)))
                    Button("sending.enqueue") { sender.enqueue() }
                        .disabled(sender.selectedAgentID == nil || !SendingPresentation.hasContent(sender.draft))
                        .accessibilityIdentifier("sending.enqueue")
                }
            }
            .navigationTitle("sending.draft")
            .toolbar { Button("sending.close") { showDraft = false } }
            .photosPicker(isPresented: $showPhotos, selection: $photo, matching: .images)
            .fileImporter(isPresented: $showFiles, allowedContentTypes: [.item]) { result in
                guard let context = fileContext, sender.attachmentImportContext == context else { return }
                do {
                    let url = try result.get()
                    guard url.startAccessingSecurityScopedResource() else {
                        importError = String(localized: "sending.permission")
                        return
                    }
                    defer { url.stopAccessingSecurityScopedResource() }
                    importError = nil
                    sender.stageAttachment(source: url, context: context, contentType: contentType(url))
                } catch { importError = error.localizedDescription }
            }
            .onChange(of: photo) { _, selection in
                guard let selection, let context = photoContext,
                      sender.attachmentImportContext == context else { return }
                let importID = UUID()
                photoImportID = importID
                Task { @MainActor in
                    defer {
                        if photoImportID == importID {
                            photo = nil
                            photoImportID = nil
                        }
                    }
                    do {
                        guard let file = try await selection.loadTransferable(type: SendingPhoto.self) else {
                            if photoImportID == importID, sender.attachmentImportContext == context {
                                importError = String(localized: "sending.photo.unavailable")
                            }
                            return
                        }
                        defer { try? FileManager.default.removeItem(at: file.url) }
                        guard photoImportID == importID, sender.attachmentImportContext == context else { return }
                        importError = nil
                        sender.stageAttachment(source: file.url, context: context, contentType: contentType(file.url))
                    } catch {
                        if photoImportID == importID, sender.attachmentImportContext == context {
                            importError = error.localizedDescription
                        }
                    }
                }
            }
        }
    }

    private var queue: some View {
        NavigationStack {
            List {
                Text("sending.receipt.explanation").font(.caption)
                ForEach(sender.entries) { entry in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(entry.draft.text).lineLimit(3)
                        Text(LocalizedStringKey(SendingPresentation.stateKey(entry.state)))
                            .accessibilityIdentifier("sending.state.\(entry.state.rawValue)")
                        Text(entry.requestID.uuidString).font(.caption2).textSelection(.enabled)
                        ForEach(entry.draft.attachments) { attachment in Text(attachment.name).font(.caption) }
                        if let error = entry.error { Text(error).font(.caption) }
                        if entry.state == .unknown { Text("sending.unknown.explanation").font(.caption) }
                        HStack {
                            if SendingPresentation.canRetry(entry.state) {
                                Button(LocalizedStringKey(entry.state == .unknown ? "sending.retry.same" : "sending.retry")) {
                                    sender.retry(requestID: entry.requestID)
                                }
                                .disabled(!sender.canRetry(requestID: entry.requestID))
                            }
                            Button("sending.delete", role: .destructive) { sender.delete(requestID: entry.requestID) }
                                .disabled(entry.state == .sending)
                        }
                    }
                }
            }
            .navigationTitle("sending.queue")
            .toolbar { Button("sending.close") { showQueue = false } }
        }
    }

    private func contentType(_ url: URL) -> String {
        UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
    }
}
