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
        ScrollView {
            contents
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
        }
        .accessibilityIdentifier("diagnostics.content")
        .navigationTitle("diagnostics.title")
        .onChange(of: connection.identity) { _, _ in
            confirmation = nil
            confirming = false
            report = ""
            outcome = nil
        }
    }

    private var contents: some View {
        VStack(alignment: .leading, spacing: 16) {
            LocalizedMultilineText(key: "diagnostics.allowlist")
            Button { prepare() } label: {
                Text("diagnostics.prepare").font(.body)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .buttonStyle(.bordered)
            .accessibilityIdentifier("diagnostics.prepare")
            if !report.isEmpty {
                Text(report).font(.body.monospaced()).textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("diagnostics.report")
                ShareLink(item: report) {
                    Label("diagnostics.export", systemImage: "square.and.arrow.up")
                        .font(.body)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("diagnostics.export")
                if let sender, let agentID = sender.selectedAgentID,
                   let context = sender.externalContext(agentID: agentID) {
                    Button("diagnostics.send") {
                        confirmation = DiagnosticConfirmation(requestID: UUID(), text: report, context: context)
                        confirming = true
                    }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("diagnostics.send")
                    .confirmationDialog("diagnostics.confirm", isPresented: $confirming) {
                        Button("diagnostics.confirmSend") { enqueue(sender: sender) }
                            .accessibilityIdentifier("diagnostics.confirmSend")
                        Button("common.cancel", role: .cancel) { confirmation = nil }
                    } message: {
                        Text(reader.agents.first(where: { $0.id == agentID })?.name ?? agentID)
                        Text(connection.selectedProfile?.name ?? "")
                    }
                } else {
                    Text("diagnostics.chooseAgent")
                        .font(.body)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("diagnostics.chooseAgent")
                }
            }
            if let outcome { Text(LocalizedStringKey(outcome)).font(.caption) }
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
