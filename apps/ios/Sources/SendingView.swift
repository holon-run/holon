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

private struct LegacyDraftModelSection: View {
    let sender: SendingCoordinator
    let onSend: () -> Void

    var body: some View {
        if let model = sender.draft.modelID {
            let token = sender.draftScopeRevision
            Section("sending.model.legacyDraft") {
                Text(verbatim: model)
                Text("sending.model.legacyDraft.explanation")
                Button("sending.model.legacyDraft.remove") { sender.removeLegacyDraftModel(revision: token) }
                if SendingPresentation.hasContent(sender.draft) {
                    Button("sending.model.legacyDraft.send") {
                        sender.enqueue(legacyModelConfirmedAt: token)
                        onSend()
                    }
                    .disabled(!sender.canEnqueueDraft)
                }
            }
        }
    }
}

private struct AgentModelSheet: View {
    let sender: SendingCoordinator
    let reader: ReadingCoordinator
    @Environment(\.dismiss) private var dismiss
    @State private var configuration: AgentModelConfiguration?
    @State private var models: [SendingModel] = []
    @State private var modelID: String?
    @State private var reasoningEffort: String?
    @State private var loadedKey: LoadKey?
    @State private var loadError = false
    @State private var catalogError = false
    @State private var applyError = false
    @State private var reload = 0

    private struct LoadKey: Equatable {
        let revision: Int?
        let liveConnectionRevision: Int
        let modelReadRevision: Int
        let reload: Int
    }

    private var modelAccessRevision: Int? {
        // An authenticated sender can outlive the reader's live connection.
        reader.status == .live ? sender.modelAccessRevision : nil
    }

    private var currentLoadKey: LoadKey {
        LoadKey(revision: modelAccessRevision, liveConnectionRevision: reader.liveConnectionRevision,
                modelReadRevision: sender.modelReadRevision, reload: reload)
    }

    var body: some View {
        let key = currentLoadKey
        NavigationStack {
            Form {
                LegacyDraftModelSection(sender: sender) { dismiss() }
                if sender.hasLegacyModelInQueue {
                    Section { Text("sending.model.legacyQueue.block") }
                }
                if key.revision != nil {
                    if sender.isUpdatingAgentModel || loadedKey != key {
                        ProgressView()
                    } else {
                        if let configuration {
                            Section("sending.model.current") {
                                Text(verbatim: configuration.effectiveModel)
                                    .accessibilityIdentifier("sending.model.effective")
                                if let source = configuration.source {
                                    LabeledContent("sending.model.source") {
                                        switch source {
                                        case "runtime_default": Text("sending.model.source.runtime_default")
                                        case "agent_override": Text("sending.model.source.agent_override")
                                        default: Text(verbatim: source)
                                        }
                                    }
                                    .accessibilityIdentifier("sending.model.source")
                                }
                                if let effort = configuration.reasoningEffort {
                                    LabeledContent("sending.model.reasoning") { Text(verbatim: effort) }
                                }
                                if let tier = configuration.serviceTier {
                                    LabeledContent("sending.model.serviceTier") { Text(verbatim: tier) }
                                }
                                if let active = configuration.activeModel, active != configuration.effectiveModel {
                                    LabeledContent("sending.model.active") { Text(verbatim: active) }
                                }
                            }
                            selectionSection(configuration, key: key)
                        }
                        if loadError || catalogError || applyError {
                            Section {
                                if loadError { Text("sending.model.loadError") }
                                if catalogError { Text("sending.model.catalogError") }
                                if applyError { Text("sending.model.applyError") }
                                Button("reading.retry") {
                                    if let state = configuration { show(state) }
                                    applyError = false
                                    reload += 1
                                }
                            }
                        }
                    }
                } else {
                    Section { Text("sending.model.offline").accessibilityIdentifier("sending.model.offline") }
                }
            }
            .navigationTitle("sending.model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("sending.close") { dismiss() }
                        .accessibilityIdentifier("sending.model.close")
                }
            }
        }
        .onChange(of: key.revision) { _, _ in applyError = false }
        .task(id: key) { await load(key) }
    }

    private func selectionSection(_ current: AgentModelConfiguration, key: LoadKey) -> some View {
        Section("sending.model.selection") {
            Picker("sending.model", selection: Binding(get: { modelID }, set: { next in
                guard next != modelID else { return }
                modelID = next
                reasoningEffort = current.initialReasoningEffort(for: next)
            })) {
                Text("sending.model.automatic").tag(nil as String?)
                ForEach(models) { model in
                    Text(verbatim: modelName(model)).tag(Optional(model.id))
                }
                if let override = current.overrideModel, !models.contains(where: { $0.id == override }) {
                    Text(verbatim: override).tag(Optional(override))
                }
            }
            .accessibilityIdentifier("sending.model.options")
            if let model = models.first(where: { $0.id == modelID }), !model.reasoningEfforts.isEmpty {
                Picker("sending.model.reasoning", selection: $reasoningEffort) {
                    Text("sending.model.default").tag(nil as String?)
                    ForEach(model.reasoningEfforts, id: \.self) { Text(verbatim: $0).tag(Optional($0)) }
                    if let reasoningEffort, !model.reasoningEfforts.contains(reasoningEffort) {
                        Text(verbatim: reasoningEffort).tag(Optional(reasoningEffort))
                    }
                }
                .accessibilityIdentifier("sending.model.reasoning")
            }
            if current.serviceTier != nil && modelID != current.overrideModel {
                Text("sending.model.serviceTier.removed")
                    .font(.caption)
                    .accessibilityIdentifier("sending.model.serviceTier.removed")
            }
            Button {
                let requested = current.selection(modelID: modelID, reasoningEffort: reasoningEffort)
                Task { await apply(requested, key: key) }
            } label: { Text("sending.model.apply") }
            .accessibilityIdentifier("sending.model.apply")
            .disabled(modelID == current.overrideModel && (modelID == nil || reasoningEffort == current.reasoningEffort)
                      || modelID != nil && !models.contains(where: { $0.id == modelID }))
        }
        .disabled(!sender.canChangeAgentModel)
    }

    private func modelName(_ model: SendingModel) -> String {
        models.filter { $0.name == model.name }.count > 1 ? "\(model.name) · \(model.id)" : model.name
    }

    private func load(_ key: LoadKey) async {
        guard let token = key.revision, key == currentLoadKey, !sender.isUpdatingAgentModel else { return }
        async let current = try? sender.readAgentModel(revision: token)
        async let catalog = try? sender.readModelCatalog(revision: token)
        let (state, choices) = await (current, catalog)
        guard !Task.isCancelled, key == currentLoadKey, !sender.isUpdatingAgentModel else { return }
        models = choices ?? []
        catalogError = choices == nil
        loadError = state == nil
        if let state { show(state) } else { configuration = nil }
        loadedKey = key
    }

    private func show(_ state: AgentModelConfiguration) {
        configuration = state
        modelID = state.overrideModel
        reasoningEffort = state.reasoningEffort
    }

    private func apply(_ selection: AgentModelSelection, key: LoadKey) async {
        guard let token = key.revision, loadedKey == key, currentLoadKey == key,
              sender.canChangeAgentModel else { return }
        applyError = false
        do {
            _ = try await sender.applyAgentModel(selection, revision: token)
        } catch {
            guard modelAccessRevision == token, reader.liveConnectionRevision == key.liveConnectionRevision else { return }
            applyError = true
        }
    }
}

struct SendingView: View {
    let sender: SendingCoordinator
    let reader: ReadingCoordinator
    var currentRunID: String? = nil
    @FocusState private var focused: Bool
    @State private var showDraft = false
    @State private var showQueue = false
    @State private var showModels = false
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
    @State private var pendingQueueDeleteID: UUID?
    @Environment(\.scenePhase) private var scenePhase

    private var hasAttentionEntries: Bool {
        sender.entries.contains { $0.state == .failed || $0.state == .unknown }
            || sender.draft.modelID != nil
            || sender.hasLegacyModelInQueue
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !sender.draft.attachments.isEmpty || (sender.draft.modelID?.isEmpty == false) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        if let modelID = sender.draft.modelID, !modelID.isEmpty {
                            let token = sender.draftScopeRevision
                            HStack(spacing: 5) {
                                Image(systemName: "exclamationmark.circle")
                                    .foregroundStyle(Color.orange)
                                Text(verbatim: modelID)
                                    .lineLimit(1)
                                Button {
                                    sender.removeLegacyDraftModel(revision: token)
                                } label: {
                                    Image(systemName: "xmark.circle.fill")
                                        .foregroundStyle(.secondary)
                                }
                                .frame(minWidth: 44, minHeight: 44)
                                .accessibilityLabel(Text("sending.model.reset"))
                            }
                            .font(.caption.weight(.medium))
                            .padding(.leading, 10).padding(.trailing, 4).padding(.vertical, 3)
                            .background(Color.orange.opacity(0.14), in: Capsule())
                        }
                        ForEach(sender.draft.attachments) { attachment in
                            HStack(spacing: 6) {
                                Image(systemName: attachment.contentType.hasPrefix("image/") ? "photo" : "doc")
                                    .foregroundStyle(Color.accentColor)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(verbatim: attachment.name).lineLimit(1)
                                    Text(verbatim: attachment.contentType + " · " + ByteCountFormatter.string(fromByteCount: attachment.byteCount, countStyle: .file))
                                        .font(.caption2).foregroundStyle(.secondary)
                                }
                                Button { sender.removeDraftAttachment(attachment.id) } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary) }
                                    .frame(minWidth: 44, minHeight: 44)
                                    .accessibilityLabel(Text("sending.remove"))
                            }.font(.caption).padding(.leading, 10).padding(.trailing, 4).padding(.vertical, 4).background(.quaternary, in: Capsule())
                        }
                    }
                }
            }
            if let cameraErrorKey {
                Text(LocalizedStringKey(cameraErrorKey)).font(.caption).foregroundStyle(.secondary).lineLimit(3)
            } else if let error = importError ?? sender.error {
                Text(verbatim: error).font(.caption).foregroundStyle(.secondary).lineLimit(3)
            }
            HStack(alignment: .bottom, spacing: 8) {
                Menu {
                    Button { selectPhoto() } label: { Label("sending.photo", systemImage: "photo") }
                    Button { selectCamera() } label: { Label("sending.camera", systemImage: "camera") }
                    Button { selectFile() } label: { Label("sending.file", systemImage: "doc") }
                    Divider()
                    Button {
                        focused = false; showModels = true
                    } label: {
                        Label("sending.model", systemImage: sender.draft.modelID == nil && !sender.hasLegacyModelInQueue ? "cpu" : "exclamationmark.circle")
                    }
                    .accessibilityIdentifier("sending.model")
                    Button {
                        focused = false; showDraft = true
                    } label: {
                        Label("sending.expandEditor", systemImage: "arrow.up.left.and.arrow.down.right")
                    }
                    Button {
                        focused = false; showQueue = true
                    } label: {
                        Label("sending.queue", systemImage: "tray.full")
                    }
                    .accessibilityIdentifier("sending.queue")
                    if currentRunID != nil {
                        Divider()
                        Button(role: .destructive) {
                            stopRunID = currentRunID
                            showStop = stopRunID != nil
                        } label: {
                            Label("sending.stop", systemImage: "stop.circle")
                        }
                        .disabled(sender.status != .ready)
                        .accessibilityIdentifier("sending.stop")
                    }
                } label: {
                    ZStack(alignment: .topTrailing) {
                        Image(systemName: "plus.circle.fill")
                            .symbolRenderingMode(.hierarchical)
                            .font(.title2)
                            .frame(width: 44, height: 44)
                        if hasAttentionEntries {
                            Circle()
                                .fill(Color.orange)
                                .frame(width: 8, height: 8)
                                .offset(x: -7, y: 8)
                        }
                    }
                }
                .accessibilityLabel(Text("sending.options"))
                .accessibilityIdentifier("sending.options")
                TextField("sending.placeholder", text: Binding(
                    get: { sender.draft.text },
                    set: { sender.editDraft(text: $0, modelID: sender.draft.modelID) }
                ), axis: .vertical)
                .lineLimit(1...5)
                .focused($focused)
                .id(sender.selectedAgentID)
                .disabled(!sender.canPersistDraft)
                .padding(.vertical, 10)
                .accessibilityLabel(Text("sending.text"))
                .accessibilityIdentifier("sending.text")
                Button {
                    if sender.draft.modelID != nil { focused = false; showModels = true }
                    else { sender.enqueue() }
                } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title).frame(width: 44, height: 44)
                }
                .disabled(!sender.canEnqueueDraft || !SendingPresentation.hasContent(sender.draft))
                .accessibilityLabel(Text("sending.send"))
                .accessibilityIdentifier("sending.enqueue")
            }
            .padding(.horizontal, 4)
            .background(Color(uiColor: .tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 20))
            .overlay(RoundedRectangle(cornerRadius: 20).strokeBorder(Color.primary.opacity(0.08), lineWidth: 0.5))
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("sending.composer")
        .background(attachmentHandlers)
        .sheet(isPresented: $showDraft) { draftEditor }
        .sheet(isPresented: $showQueue) { queue }
        .sheet(isPresented: $showModels) { AgentModelSheet(sender: sender, reader: reader) }
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
            showModels = false
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
                }
                LegacyDraftModelSection(sender: sender) { showDraft = false }
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
                        .disabled(!sender.canEnqueueDraft || sender.draft.modelID != nil
                                  || !SendingPresentation.hasContent(sender.draft))
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
            Group {
                if sender.entries.isEmpty {
                    ContentUnavailableView {
                        Label("sending.queue.empty", systemImage: "tray")
                    } description: {
                        Text("sending.receipt.explanation")
                    }
                } else {
                    List {
                        Text("sending.receipt.explanation").font(.caption).foregroundStyle(.secondary)
                        ForEach(sender.entries) { entry in
                            VStack(alignment: .leading, spacing: 8) {
                                Text(entry.draft.text).lineLimit(3)
                                Text(LocalizedStringKey(SendingPresentation.stateKey(entry.state)))
                                    .font(.caption.weight(.medium))
                                    .foregroundStyle(entry.state == .failed ? .red : entry.state == .unknown ? .orange : .secondary)
                                    .accessibilityIdentifier("sending.state.\(entry.state.rawValue)")
                                Text(entry.requestID.uuidString).font(.caption2).foregroundStyle(.tertiary).textSelection(.enabled)
                                    .accessibilityIdentifier("sending.requestID." + entry.requestID.uuidString)
                                ForEach(entry.draft.attachments) { attachment in Text(attachment.name).font(.caption) }
                                if let model = entry.legacyModelID, entry.state != .received {
                                    Text("sending.model.legacyQueue.item").font(.caption)
                                    Text(verbatim: model).font(.caption)
                                }
                                if let error = entry.error { Text(error).font(.caption).foregroundStyle(.red) }
                                if entry.state == .unknown { Text("sending.unknown.explanation").font(.caption).foregroundStyle(.secondary) }
                                HStack(spacing: 16) {
                                    if SendingPresentation.canRetry(entry.state) {
                                        SendingEntryRetryButton(sender: sender, entry: entry)
                                            .accessibilityIdentifier("sending.retry." + entry.requestID.uuidString)
                                    }
                                    Button("sending.delete", role: .destructive) {
                                        // Match LocalMessageView: an unknown receipt may
                                        // have been accepted server-side; dropping its
                                        // retry handle deserves an explicit confirm.
                                        if entry.state == .unknown { pendingQueueDeleteID = entry.requestID }
                                        else { sender.delete(requestID: entry.requestID) }
                                    }
                                    .disabled(entry.state == .sending)
                                }
                                .buttonStyle(.borderless)
                            }
                            .padding(.vertical, 4)
                        }
                    }
                }
            }
            .navigationTitle("sending.queue")
            .toolbar { Button("sending.close") { showQueue = false } }
        }
        .confirmationDialog("sending.delete.unknown.confirm", isPresented: Binding(
            get: { pendingQueueDeleteID != nil },
            set: { if !$0 { pendingQueueDeleteID = nil } }
        ), titleVisibility: .visible) {
            Button("sending.delete", role: .destructive) {
                if let pendingQueueDeleteID { sender.delete(requestID: pendingQueueDeleteID) }
                pendingQueueDeleteID = nil
            }
            Button("common.cancel", role: .cancel) { pendingQueueDeleteID = nil }
        } message: {
            Text("sending.delete.unknown.explanation")
        }
    }

    private func contentType(_ url: URL) -> String {
        UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
    }
}
