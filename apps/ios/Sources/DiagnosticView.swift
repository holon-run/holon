import SwiftUI

private struct DiagnosticConfirmation {
    let requestID: UUID
    let text: String
    let context: SendingExternalContext
}

struct DiagnosticView: View {
    let connection: ConnectionCoordinator
    let reader: ReadingCoordinator
    let sender: SendingCoordinator?
    @State private var report = ""
    @State private var confirmation: DiagnosticConfirmation?
    @State private var confirming = false
    @State private var outcome: String?

    var body: some View {
        Section("diagnostics.title") {
            Text("diagnostics.allowlist").font(.caption).foregroundStyle(.secondary)
            Button("diagnostics.prepare") { prepare() }
            if !report.isEmpty {
                Text(report).font(.caption.monospaced()).textSelection(.enabled)
                ShareLink(item: report) { Label("diagnostics.export", systemImage: "square.and.arrow.up") }
                if let sender, let agentID = sender.selectedAgentID,
                   let context = sender.externalContext(agentID: agentID) {
                    Button("diagnostics.send") {
                        confirmation = DiagnosticConfirmation(requestID: UUID(), text: report, context: context)
                        confirming = true
                    }
                    .confirmationDialog("diagnostics.confirm", isPresented: $confirming) {
                        Button("diagnostics.confirmSend") { enqueue(sender: sender) }
                        Button("common.cancel", role: .cancel) { confirmation = nil }
                    } message: {
                        Text(reader.agents.first(where: { $0.id == agentID })?.name ?? agentID)
                        Text(connection.selectedProfile?.name ?? "")
                    }
                } else {
                    Text("diagnostics.chooseAgent").font(.caption)
                }
            }
            if let outcome { Text(LocalizedStringKey(outcome)).font(.caption) }
        }
        .onChange(of: connection.identity) { _, _ in
            confirmation = nil
            confirming = false
            report = ""
            outcome = nil
        }
    }

    private func prepare() {
        do {
            report = try DiagnosticReport(connection: connection.status, reading: reader.status,
                                          sending: sender?.status ?? .disconnected,
                                          agentCount: reader.agents.count, entries: sender?.entries ?? []).text()
            outcome = nil
        } catch { outcome = "diagnostics.failed" }
    }

    private func enqueue(sender: SendingCoordinator) {
        guard let confirmation else { return }
        defer { self.confirmation = nil }
        do {
            try sender.enqueueExternal(requestID: confirmation.requestID, text: confirmation.text,
                                       context: confirmation.context, sendNow: true)
            outcome = "diagnostics.queued"
        } catch { outcome = "diagnostics.failed" }
    }
}
