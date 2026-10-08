import Foundation
import HolonClient
import Observation

struct SendingAttachmentImportContext: Equatable {
    fileprivate let scope: SendingScope
    fileprivate let revision: Int
}

struct SendingExternalContext: Equatable {
    fileprivate let scope: SendingScope
    fileprivate let connectionRevision: Int
    fileprivate let confirmationRevision: Int
}

@MainActor @Observable
final class SendingCoordinator {
    private(set) var draft = SendingDraft()
    private(set) var entries: [SendingEntry] = []
    private(set) var models: [SendingModel] = []
    private(set) var status: SendingStatus = .disconnected
    private(set) var error: String?
    private(set) var selectedAgentID: String?
    var onConnectionFailure: ((HolonHTTPFailure) -> Void)?
    @ObservationIgnored private var identity: HolonConnectionIdentity?
    @ObservationIgnored private let store: SendingStore
    @ObservationIgnored private var transport: (any SendingTransport)?
    @ObservationIgnored private var partition: ReadingPartition?
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var connectionRevision = 0
    @ObservationIgnored private var confirmationRevision = 0
    @ObservationIgnored private var attachmentRevision = 0
    @ObservationIgnored private var catalogRevision = 0
    @ObservationIgnored private var foreground = true
    @ObservationIgnored private var sendTask: Task<Void, Never>?
    @ObservationIgnored private var activeRequestID: UUID?
    @ObservationIgnored private var catalogTask: Task<Void, Never>?

    init(store: SendingStore) { self.store = store }
    private var scope: SendingScope? {
        guard let partition, let selectedAgentID else { return nil }
        return SendingScope(partition: partition, agentID: selectedAgentID)
    }

    var attachmentImportContext: SendingAttachmentImportContext? {
        guard let scope else { return nil }
        return SendingAttachmentImportContext(scope: scope, revision: attachmentRevision)
    }

    func activate(client: HolonClient, identity: HolonConnectionIdentity, apiBaseURL: URL) {
        activate(transport: SendingClientTransport(client: client, authority: identity),
                 identity: identity, apiBaseURL: apiBaseURL)
    }

    func activate(transport: any SendingTransport, identity: HolonConnectionIdentity, apiBaseURL: URL) {
        disconnect()
        self.transport = transport
        self.identity = identity
        partition = ReadingPartition(apiBaseURL: apiBaseURL, identity: identity)
        status = partition == nil ? .disconnected : (foreground ? .ready : .offline)
        reload()
        loadModels()
    }

    private func loadModels() {
        guard foreground, let transport else { return }
        catalogTask?.cancel()
        catalogRevision += 1
        let token = catalogRevision
        catalogTask = Task { [weak self] in
            do {
                let catalog = try await transport.models()
                guard let self, self.catalogRevision == token, self.foreground, !Task.isCancelled else { return }
                self.models = catalog
            } catch {
                guard let self, self.catalogRevision == token, self.foreground, !Task.isCancelled else { return }
                self.error = error.localizedDescription
                self.reportConnectionFailure(error)
            }
        }
    }

    func disconnect() {
        let previous = transport
        cancelLocalSend()
        revision += 1
        connectionRevision += 1
        confirmationRevision += 1
        attachmentRevision += 1
        catalogRevision += 1
        catalogTask?.cancel()
        catalogTask = nil
        transport = nil
        identity = nil
        partition = nil
        draft = SendingDraft()
        entries = []
        models = []
        status = .disconnected
        if let previous { Task { await previous.close() } }
    }

    func setForeground(_ value: Bool) {
        let changed = foreground != value
        foreground = value
        if !value {
            confirmationRevision += 1
            cancelLocalSend()
            revision += 1
            catalogRevision += 1
            catalogTask?.cancel()
            catalogTask = nil
        } else if changed {
            loadModels()
        }
        status = transport == nil ? .disconnected : (value ? .ready : .offline)
    }

    func selectAgent(_ agentID: String?) {
        cancelLocalSend()
        revision += 1
        attachmentRevision += 1
        selectedAgentID = agentID
        reload()
    }

    func editDraft(text: String, modelID: String?) {
        guard let scope else { return }
        var next = draft
        next.text = text
        next.modelID = modelID
        perform { try store.saveDraft(next, scope: scope) }
    }

    func removeDraftAttachment(_ id: UUID) {
        guard let scope else { return }
        var next = draft
        next.attachments.removeAll { $0.id == id }
        perform { try store.saveDraft(next, scope: scope) }
    }

    func stageAttachment(source: URL, context: SendingAttachmentImportContext,
                         contentType: String = "application/octet-stream") {
        guard attachmentImportContext == context else { return }
        perform { try store.stage(source: source, scope: context.scope, contentType: contentType) }
    }

    func enqueue() {
        guard let scope else { return }
        do {
            let entry = try store.enqueue(scope)
            attachmentRevision += 1
            error = nil
            reload()
            deliver(entry)
        } catch { self.error = error.localizedDescription; reload() }
    }

    /// A canonical receipt stays joined even when that input leaves the history window.
    /// This marker is presentation metadata, never a read cursor or an execution result.
    func observeCanonicalInputs(_ ids: Set<String>) {
        for var entry in entries where entry.canonicalObserved != true {
            guard let messageID = entry.messageID, ids.contains(messageID) else { continue }
            entry.canonicalObserved = true
            try? store.update(entry)
        }
        reload()
    }

    func externalContext(agentID: String) -> SendingExternalContext? {
        guard transport != nil, identity != nil, foreground, let partition,
              !agentID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return SendingExternalContext(scope: SendingScope(partition: partition, agentID: agentID),
                                      connectionRevision: connectionRevision,
                                      confirmationRevision: confirmationRevision)
    }

    /// Host confirmation queues an isolated request; imports are never background-auto-sent.
    @discardableResult
    func enqueueExternal(requestID: UUID, text: String, attachments: [SendingPreparedAttachment] = [],
                         context: SendingExternalContext, sendNow: Bool = false) throws -> UUID {
        guard foreground, transport != nil, identity != nil,
              context.connectionRevision == connectionRevision,
              context.confirmationRevision == confirmationRevision,
              context.scope.partition == partition,
              !sendNow || context.scope == scope else { throw SendingFailure.unavailable }
        let entry = try store.enqueueExternal(requestID: requestID, scope: context.scope,
                                              text: text, attachments: attachments)
        error = nil
        reload()
        if sendNow, entry.state == .queued { deliver(entry) }
        return entry.requestID
    }

    func canRetry(requestID: UUID) -> Bool {
        guard transport != nil, identity != nil, foreground, activeRequestID == nil,
              let entry = entries.first(where: { $0.requestID == requestID }),
              entry.scope == scope else { return false }
        // Offline records the last failure, not whether an explicit attempt is allowed.
        return entry.state == .unknown || entry.state == .failed || entry.state == .queued
    }

    func retry(requestID: UUID) {
        guard canRetry(requestID: requestID),
              let entry = entries.first(where: { $0.requestID == requestID }) else { return }
        deliver(entry)
    }

    func delete(requestID: UUID) {
        guard let scope, activeRequestID != requestID else { return }
        perform { try store.delete(requestID: requestID, scope: scope) }
    }

    /// Only this explicit action calls the runtime stop API. Navigation/cancellation never does.
    func stopAgent(runID: String) async {
        guard let transport, let scope, foreground else { error = SendingFailure.unavailable.localizedDescription; return }
        let token = revision
        do { try await transport.stop(agentID: scope.agentID, runID: runID) }
        catch {
            if revision == token {
                self.error = error.localizedDescription
                reportConnectionFailure(error)
            }
        }
    }

    private func reportConnectionFailure(_ error: any Error) {
        guard let failure = error as? HolonHTTPFailure, failure.identity == identity,
              failure.statusCode == 401 || failure.statusCode == 403 else { return }
        onConnectionFailure?(failure)
    }

    private func perform(_ operation: () throws -> Void) {
        do { try operation(); error = nil } catch { self.error = error.localizedDescription }
        reload()
    }

    private func reload() {
        guard let scope else { draft = SendingDraft(); entries = []; return }
        draft = store.draft(scope)
        entries = store.entries(scope)
    }

    private func cancelLocalSend() {
        sendTask?.cancel()
        sendTask = nil
        if let id = activeRequestID, let scope,
           var entry = store.entries(scope).first(where: { $0.requestID == id }), entry.state == .sending {
            entry.state = .unknown
            entry.error = "Local send cancelled; receipt unknown. Retry explicitly."
            do { try store.update(entry) } catch { self.error = error.localizedDescription }
        }
        activeRequestID = nil
        status = transport == nil ? .disconnected : (foreground ? .ready : .offline)
        reload()
    }

    private func deliver(_ original: SendingEntry) {
        guard activeRequestID == nil else { return }
        guard let transport, foreground, original.scope == scope else {
            status = .offline
            error = SendingFailure.unavailable.localizedDescription
            return
        }
        guard original.state != .received else { return }
        let token = revision
        activeRequestID = original.requestID
        status = .sending
        sendTask = Task { [weak self] in
            guard let self, self.revision == token, self.scope == original.scope,
                  self.foreground, self.activeRequestID == original.requestID,
                  !Task.isCancelled else { return }
            var entry = original
            do {
                entry.state = .sending
                entry.error = nil
                try self.store.update(entry)
                self.reload()
                if entry.payload == nil {
                    for attachment in entry.draft.attachments {
                        if entry.uploaded[attachment.id] != nil { continue }
                        try self.store.validate(attachment)
                        let uploaded = try await transport.upload(agentID: entry.scope.agentID,
                            attachment: attachment, file: self.store.attachmentURL(attachment))
                        guard self.revision == token, !Task.isCancelled else { return }
                        entry.uploaded[attachment.id] = uploaded
                        try self.store.update(entry)
                    }
                    entry.payload = SendingPayload(text: entry.draft.text, modelID: entry.draft.modelID,
                        attachments: entry.draft.attachments.compactMap { entry.uploaded[$0.id] })
                    try self.store.update(entry)
                }
                guard self.revision == token, !Task.isCancelled, let payload = entry.payload else { return }
                let receipt = try await transport.send(agentID: entry.scope.agentID,
                                                      requestID: entry.requestID, payload: payload)
                guard self.revision == token, !Task.isCancelled else { return }
                entry.state = .received
                entry.messageID = receipt
                try self.store.update(entry)
                self.status = .ready
            } catch {
                guard self.revision == token, !Task.isCancelled else { return }
                entry.state = error is SendingFailure ? .failed : .unknown
                entry.error = error.localizedDescription
                do { try self.store.update(entry) } catch { self.error = error.localizedDescription }
                self.status = error is SendingFailure ? .ready : .offline
                self.reportConnectionFailure(error)
            }
            guard self.revision == token else { return }
            self.activeRequestID = nil
            self.sendTask = nil
            self.reload()
        }
    }
}
