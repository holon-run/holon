import SwiftUI

struct SystemExperienceView: View {
    let connection: ConnectionCoordinator
    let reader: ReadingCoordinator
    let sender: SendingCoordinator?
    @Bindable var imports: SharedImportCoordinator
    @Environment(\.scenePhase) private var scenePhase
    @State private var preview: SharedImportPayload?
    @State private var target = ""
    @State private var confirming = false

    var body: some View {
        Form {
            Section("share.inbox") {
                Text("share.hostHelp").font(.caption).foregroundStyle(.secondary)
                Button("share.reload") { imports.reload() }
                if !imports.available { Text("share.unavailable") }
                else if imports.pending.isEmpty { Text("share.empty") }
                ForEach(imports.pending) { payload in
                    Button {
                        preview = payload
                        target = sender?.selectedAgentID ?? ""
                    } label: {
                        VStack(alignment: .leading) {
                            Text(payload.text.isEmpty ? payload.urls.first?.absoluteString ?? "" : payload.text)
                                .lineLimit(2)
                            Text("\(payload.attachments.count)").font(.caption)
                        }
                    }
                    .swipeActions {
                        Button("share.discard", role: .destructive) { imports.discard(payload.id) }
                    }
                }
                if let outcome = imports.outcome { Text(LocalizedStringKey(outcome)).font(.caption) }
            }
        }
        .navigationTitle("settings.sharing")
        .onAppear { imports.reload() }
        .sheet(item: $preview, onDismiss: {
            imports.revokeConfirmation()
            confirming = false
        }) { payload in
            NavigationStack {
                Form {
                    Section("share.preview") {
                        if !payload.text.isEmpty { Text(payload.text).textSelection(.enabled) }
                        ForEach(Array(payload.urls.enumerated()), id: \.offset) { _, url in
                            Text(url.absoluteString).textSelection(.enabled)
                        }
                        ForEach(payload.attachments, id: \.id) { attachment in
                            LabeledContent(attachment.name, value: ByteCountFormatter.string(
                                fromByteCount: Int64(attachment.byteCount), countStyle: .file))
                        }
                    }
                    Section("share.destination") {
                        Text(connection.selectedProfile?.name ?? "")
                        Picker("share.agent", selection: $target) {
                            Text("share.chooseAgent").tag("")
                            ForEach(reader.agents) { agent in Text(agent.name).tag(agent.id) }
                        }
                        Text("share.queueHelp").font(.caption).foregroundStyle(.secondary)
                        Button("share.confirmQueue") {
                            guard let sender else { return }
                            imports.prepare(payloadID: payload.id, agentID: target, sender: sender)
                            confirming = imports.confirmation != nil
                        }
                        .disabled(sender?.externalContext(agentID: target) == nil)
                        .confirmationDialog("share.confirmDestination", isPresented: $confirming) {
                            Button("share.confirmQueue") {
                                if let sender { imports.confirm(sender: sender) }
                                preview = nil
                            }
                            Button("common.cancel", role: .cancel) { imports.revokeConfirmation() }
                        } message: {
                            Text(reader.agents.first(where: { $0.id == target })?.name ?? target)
                            Text(connection.selectedProfile?.name ?? "")
                        }
                    }
                }
                .navigationTitle("share.preview")
                .toolbar { Button("common.cancel") { preview = nil; imports.revokeConfirmation() } }
            }
        }
        .onChange(of: connection.identity) { _, _ in revoke() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { imports.reload() } else { revoke() }
        }
    }

    private func revoke() {
        imports.revokeConfirmation()
        confirming = false
        preview = nil
        target = ""
    }
}
