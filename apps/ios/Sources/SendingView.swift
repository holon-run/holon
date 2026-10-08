import CoreTransferable
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import AVFoundation

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
    var commonModels: [String] = []
    @FocusState private var focused: Bool
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
    @State private var cameraErrorKey: String?
    @State private var showCamera = false
    @State private var cameraContext: SendingAttachmentImportContext?
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !sender.draft.attachments.isEmpty {
                ScrollView(.horizontal) {
                    HStack {
                        ForEach(sender.draft.attachments) { attachment in
                            HStack(spacing: 6) {
                                Image(systemName: attachment.contentType.hasPrefix("image/") ? "photo" : "doc")
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(verbatim: attachment.name).lineLimit(1)
                                    Text(verbatim: attachment.contentType + " · " + ByteCountFormatter.string(fromByteCount: attachment.byteCount, countStyle: .file))
                                        .font(.caption2).foregroundStyle(.secondary)
                                }
                                Button { sender.removeDraftAttachment(attachment.id) } label: { Image(systemName: "xmark.circle") }
                                    .frame(minWidth: 44, minHeight: 44)
                                    .accessibilityLabel(Text("sending.remove"))
                            }.font(.caption).padding(8).background(.quaternary, in: Capsule())
                        }
                    }
                }
            }
            if let cameraErrorKey {
                Text(LocalizedStringKey(cameraErrorKey)).font(.caption).foregroundStyle(.secondary).lineLimit(3)
            } else if let error = importError ?? sender.error {
                Text(verbatim: error).font(.caption).foregroundStyle(.secondary).lineLimit(3)
            }
            HStack(alignment: .bottom, spacing: 10) {
                Menu {
                    Button { selectPhoto() } label: { Label("sending.photo", systemImage: "photo") }
                    Button { selectCamera() } label: { Label("sending.camera", systemImage: "camera") }
                    Button { selectFile() } label: { Label("sending.file", systemImage: "doc") }
                    Button("sending.draft") { focused = false; showDraft = true }
                    Button("sending.queue") { focused = false; showQueue = true }
                        .accessibilityIdentifier("sending.queue")
                    if currentRunID != nil {
                        Button("sending.stop", role: .destructive) {
                            stopRunID = currentRunID
                            showStop = stopRunID != nil
                        }.disabled(sender.status != .ready).accessibilityIdentifier("sending.stop")
                    }
                    modelPicker
                } label: { Image(systemName: "plus.circle").font(.title2).frame(width: 44, height: 44) }
                .accessibilityLabel(Text("sending.options"))
                .accessibilityIdentifier("sending.options")
                TextField("sending.placeholder", text: Binding(
                    get: { sender.draft.text },
                    set: { sender.editDraft(text: $0, modelID: sender.draft.modelID) }
                ), axis: .vertical)
                .lineLimit(1...5)
                .focused($focused)
                .id(sender.selectedAgentID)
                .padding(.vertical, 10)
                .accessibilityLabel(Text("sending.text"))
                .accessibilityIdentifier("sending.text")
                Button { sender.enqueue() } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title).frame(width: 44, height: 44)
                }
                .disabled(sender.selectedAgentID == nil || !SendingPresentation.hasContent(sender.draft))
                .accessibilityLabel(Text("sending.send"))
                .accessibilityIdentifier("sending.enqueue")
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("sending.composer")
        .background(attachmentHandlers)
        .sheet(isPresented: $showDraft) { draftEditor }
        .sheet(isPresented: $showQueue) { queue }
        .fullScreenCover(isPresented: $showCamera) {
            CameraAttachmentView { data in
                showCamera = false
                guard let context = cameraContext, sender.attachmentImportContext == context else { return }
                cameraContext = nil
                guard let data else { return }
                let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".jpg")
                defer { try? FileManager.default.removeItem(at: url) }
                do {
                    guard data.count <= 20 * 1024 * 1024 else { throw SendingFailure.oversizedAttachment("Photo") }
                    try data.write(to: url, options: [.atomic, .completeFileProtection])
                    importError = nil; cameraErrorKey = nil
                    sender.stageAttachment(source: url, context: context, contentType: "image/jpeg")
                } catch { importError = error.localizedDescription }
            }
        }
        .confirmationDialog("sending.stop.confirm", isPresented: $showStop, titleVisibility: .visible) {
            Button("sending.stop", role: .destructive) {
                if let stopRunID { Task { await sender.stopAgent(runID: stopRunID) } }
            }
        } message: {
            Text("sending.stop.explanation")
        }
        .onChange(of: sender.selectedAgentID) { _, _ in
            focused = false
            showDraft = false
            showQueue = false
            importError = nil
            cameraErrorKey = nil
            photoContext = nil
            fileContext = nil
            showCamera = false; cameraContext = nil
        }
        .onChange(of: scenePhase) { _, value in
            if value == .background { showCamera = false; cameraContext = nil }
        }
    }

    private var modelPicker: some View {
        Picker("sending.model", selection: Binding(
            get: { sender.draft.modelID ?? "" },
            set: { sender.editDraft(text: sender.draft.text, modelID: $0.isEmpty ? nil : $0) }
        )) {
            Text("sending.model.default").tag("")
            let common = sender.models.filter { commonModels.contains($0.id) || $0.id == sender.draft.modelID }
            ForEach(common) { Text(verbatim: $0.name).tag($0.id) }
            Section("sending.models.more") {
                ForEach(sender.models.filter { model in !common.contains(where: { $0.id == model.id }) }) {
                    Text(verbatim: $0.name).tag($0.id)
                }
            }
            if let selected = sender.draft.modelID, !sender.models.contains(where: { $0.id == selected }) {
                Text(verbatim: selected).tag(selected)
            }
        }
    }

    private func selectPhoto() {
        cameraErrorKey = nil; importError = nil
        photoContext = sender.attachmentImportContext
        photoImportID = nil
        showPhotos = photoContext != nil
    }
    private func selectFile() {
        cameraErrorKey = nil; importError = nil
        fileContext = sender.attachmentImportContext
        showFiles = fileContext != nil
    }
    private func selectCamera() {
        importError = nil; cameraErrorKey = nil
        guard UIImagePickerController.isSourceTypeAvailable(.camera), let context = sender.attachmentImportContext else {
            cameraErrorKey = "sending.camera.unavailable"; return
        }
        cameraContext = context
        Task { @MainActor in
            let allowed = await AVCaptureDevice.requestAccess(for: .video)
            guard cameraContext == context, sender.attachmentImportContext == context else { return }
            if allowed { importError = nil; cameraErrorKey = nil; showCamera = true }
            else { cameraContext = nil; cameraErrorKey = "sending.camera.permission" }
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
                    modelPicker
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
                        selectPhoto()
                    } label: { Label("sending.photo", systemImage: "photo") }
                    Button("sending.file") {
                        selectFile()
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
        }
    }

    private var attachmentHandlers: some View {
        Color.clear.frame(width: 0, height: 0)
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
                            .accessibilityIdentifier("sending.requestID." + entry.requestID.uuidString)
                        ForEach(entry.draft.attachments) { attachment in Text(attachment.name).font(.caption) }
                        if let error = entry.error { Text(error).font(.caption) }
                        if entry.state == .unknown { Text("sending.unknown.explanation").font(.caption) }
                        HStack {
                            if SendingPresentation.canRetry(entry.state) {
                                Button(LocalizedStringKey(entry.state == .unknown ? "sending.retry.same" : "sending.retry")) {
                                    sender.retry(requestID: entry.requestID)
                                }
                                .disabled(!sender.canRetry(requestID: entry.requestID))
                                .accessibilityIdentifier("sending.retry." + entry.requestID.uuidString)
                            }
                            Button("sending.delete", role: .destructive) { sender.delete(requestID: entry.requestID) }
                                .disabled(entry.state == .sending)
                        }
                        .buttonStyle(.borderless)
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
