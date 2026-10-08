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
    }
}
