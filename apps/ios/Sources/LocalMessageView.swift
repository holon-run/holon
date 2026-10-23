import SwiftUI
import HolonClient

enum LocalMessageProjection {
    static func canonicalIDs(_ raw: JSONValue?) -> Set<String> {
        guard let raw else { return [] }
        var ids = Set((raw["pending_inputs"]?.workArray ?? []).compactMap { $0["message_id"]?.workString })
        for turn in (raw["turns"]?.workArray ?? []) + (raw["active_turns"]?.workArray ?? []) {
            ids.formUnion((turn["inputs"]?.workArray ?? []).compactMap { $0["message_id"]?.workString })
        }
        return ids
    }
    static func visible(_ entries: [SendingEntry], canonicalIDs: Set<String>) -> [SendingEntry] {
        entries.filter { entry in
            if entry.canonicalObserved == true { return false }
            guard let id = entry.messageID else { return true }
            return !canonicalIDs.contains(id)
        }
    }
}

struct LocalMessageView: View {
    let sender: SendingCoordinator
    let canonicalIDs: Set<String>
    @State private var pendingDeleteID: UUID?
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
        ForEach(LocalMessageProjection.visible(sender.entries, canonicalIDs: canonicalIDs)) { entry in
            OperatorMessageBubble {
            VStack(alignment: .leading, spacing: 6) {
                OperatorMessageText(text: entry.draft.text)
                ForEach(entry.draft.attachments) { attachment in
                    Label(attachment.name, systemImage: "paperclip").font(.caption)
                }
                HStack {
                    Text(LocalizedStringKey(SendingPresentation.stateKey(entry.state))).font(.caption).foregroundStyle(.secondary)
                        .accessibilityIdentifier("sending.state." + entry.state.rawValue)
                    if SendingPresentation.canRetry(entry.state) {
                        Button(LocalizedStringKey(entry.state == .queued ? "sending.send" : entry.state == .unknown ? "sending.retry.same" : "sending.retry")) {
                            sender.retry(requestID: entry.requestID)
                        }.disabled(!sender.canRetry(requestID: entry.requestID)).font(.caption)
                    }
                    if entry.state != .sending && entry.state != .received {
                        Spacer()
                        Button {
                            // An unknown receipt may have been accepted server-side;
                            // dropping its retry handle deserves an explicit confirm.
                            if entry.state == .unknown { pendingDeleteID = entry.requestID }
                            else { sender.delete(requestID: entry.requestID) }
                        } label: { Image(systemName: "trash") }
                            .font(.caption)
                            .accessibilityLabel(Text("sending.delete"))
                            .accessibilityIdentifier("sending.delete." + entry.requestID.uuidString)
                    }
                }
                if entry.state == .unknown { Text("sending.unknown.explanation").font(.caption).foregroundStyle(.secondary) }
                if let error = entry.error { Text(verbatim: error).font(.caption).foregroundStyle(.secondary) }
            }
            }
            .accessibilityIdentifier("localMessage." + entry.requestID.uuidString)
        }
        }
        .onChange(of: canonicalIDs, initial: true) { _, ids in sender.observeCanonicalInputs(ids) }
        .onChange(of: sender.entries.map(\.messageID)) { _, _ in sender.observeCanonicalInputs(canonicalIDs) }
        .confirmationDialog("sending.delete.unknown.confirm", isPresented: Binding(
            get: { pendingDeleteID != nil },
            set: { if !$0 { pendingDeleteID = nil } }
        ), titleVisibility: .visible) {
            Button("sending.delete", role: .destructive) {
                if let pendingDeleteID { sender.delete(requestID: pendingDeleteID) }
                pendingDeleteID = nil
            }
            Button("common.cancel", role: .cancel) { pendingDeleteID = nil }
        } message: {
            Text("sending.delete.unknown.explanation")
        }
    }
}
