import Foundation
import HolonClient
import Observation
import UniformTypeIdentifiers

struct HostImportConfirmation {
    let payload: SharedImportPayload
    let agentID: String
    let context: SendingExternalContext
}

@MainActor @Observable
final class SharedImportCoordinator {
    private(set) var pending: [SharedImportPayload] = []
    private(set) var outcome: String?
    private(set) var confirmation: HostImportConfirmation?
    let available: Bool
    @ObservationIgnored private let store: SharedImportStore?

    init(store: SharedImportStore? = try? SharedImportStore.configured()) {
        self.store = store
        available = store != nil
    }

    func reload() {
        guard let store else { pending = []; outcome = "share.unavailable"; return }
        do { pending = try store.load() }
        catch { pending = []; outcome = "share.failed" }
    }

    func revokeConfirmation() { confirmation = nil }

    func prepare(payloadID: UUID, agentID: String, sender: SendingCoordinator) {
        confirmation = nil
        guard let payload = pending.first(where: { $0.id == payloadID }),
              let context = sender.externalContext(agentID: agentID) else {
            outcome = "share.reconfirm"
            return
        }
        confirmation = HostImportConfirmation(payload: payload, agentID: agentID, context: context)
    }

    func confirm(sender: SendingCoordinator) {
        guard let store, let confirmation else { return }
        defer { self.confirmation = nil }
        do {
            guard try store.load().first(where: { $0.id == confirmation.payload.id }) == confirmation.payload else {
                throw SharedImportError.invalidRecord
            }
            let payload = confirmation.payload
            let attachments = try payload.attachments.map { attachment in
                SendingPreparedAttachment(
                    name: attachment.name,
                    contentType: UTType(attachment.typeIdentifier)?.preferredMIMEType ?? "application/octet-stream",
                    data: try store.attachmentData(payloadID: payload.id, attachmentID: attachment.id))
            }
            let text = SharedImportStore.sendingText(text: payload.text, urls: payload.urls)
            try sender.enqueueExternal(requestID: payload.id, text: text, attachments: attachments,
                                       context: confirmation.context)
            // A cleanup failure cannot undo or duplicate an already durable submission.
            do {
                try store.consume(id: payload.id, enqueueSucceeded: true)
                outcome = "share.queued"
            } catch { outcome = "share.queuedRetained" }
            reload()
        } catch { outcome = "share.reconfirm" }
    }

    func discard(_ id: UUID) {
        guard let store else { return }
        do {
            try store.discard(id: id)
            if confirmation?.payload.id == id { confirmation = nil }
            reload()
        } catch { outcome = "share.failed" }
    }
}
