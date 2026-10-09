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
    private(set) var queuedAgentID: String?
    let available: Bool
    @ObservationIgnored private let store: SharedImportStore?
    @ObservationIgnored private var partition: ReadingPartition?

    init(store: SharedImportStore? = try? SharedImportStore.configured()) {
        self.store = store
        available = store != nil
    }

    func reload() {
        guard let store else { pending = []; outcome = "share.unavailable"; return }
        do {
            pending = try store.load().filter { payload in
                guard payload.delivery?.state != .accepted else { return false }
                guard let target = payload.delivery?.target else { return true }
                return matches(target)
            }
        }
        catch { pending = []; outcome = "share.failed" }
    }

    func revokeConfirmation() { confirmation = nil }
    func resetDestination() { confirmation = nil; queuedAgentID = nil; outcome = nil; partition = nil; reload() }
    func activate(_ partition: ReadingPartition?) { self.partition = partition; reload() }
    private func matches(_ target: SharedShareTarget) -> Bool {
        guard let partition else { return false }
        return target.networkID == partition.network && target.apiBaseURL.absoluteString == partition.api &&
            target.runtimeID == partition.runtime && target.userID == partition.user &&
            target.visibilityScopeID == partition.visibility
    }

    func prepare(payloadID: UUID, agentID: String, sender: SendingCoordinator) {
        confirmation = nil
        guard let payload = pending.first(where: { $0.id == payloadID }),
              let context = sender.externalContext(agentID: agentID) else {
            outcome = "share.reconfirm"
            return
        }
        if let target = payload.delivery?.target {
            guard target.agentID == agentID, context.scope.partition == partition, matches(target) else {
                outcome = "share.originalDestination"; return
            }
        }
        confirmation = HostImportConfirmation(payload: payload, agentID: agentID, context: context)
    }

    @discardableResult
    func confirm(sender: SendingCoordinator) -> Bool {
        guard let store, let confirmation else { return false }
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
                                       context: confirmation.context,
                                       previousOutcomeUnknown: payload.delivery?.state == .unknown)
            queuedAgentID = confirmation.agentID
            // A cleanup failure cannot undo or duplicate an already durable submission.
            do {
                try store.consume(id: payload.id, enqueueSucceeded: true)
                outcome = "share.queued"
            } catch { outcome = "share.queuedRetained" }
            reload()
            return true
        } catch { outcome = "share.reconfirm"; return false }
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
