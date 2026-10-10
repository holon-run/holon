import SwiftUI
import HolonClient

struct ContentReportView: View {
    let turnID: String
    let scope: ContentReportScope
    @Bindable var reader: ReadingCoordinator
    @Environment(\.dismiss) private var dismiss
    @State private var selectedActivityID: String?
    @State private var draft: ContentReportDraft?
    @State private var targetUnavailable = false
    @State private var confirming = false

    var body: some View {
        NavigationStack {
            Form {
                if reader.contentReportScope != scope {
                    Text("report.contextChanged")
                } else if let draft {
                    reportForm(draft)
                } else {
                    Section {
                        Text("report.selectResponse")
                        ForEach(ActivityPresentation.items(reader.activities[turnID]).filter { $0.kind == "assistant" && $0.detailID != nil }) { activity in
                            Button {
                                selectedActivityID = activity.id
                                Task { await select(activity) }
                            } label: {
                                Text(verbatim: activity.summary).lineLimit(4)
                            }
                            .disabled(selectedActivityID != nil)
                            .accessibilityIdentifier("report.target." + activity.id)
                        }
                        if selectedActivityID != nil || reader.loadingActivities.contains(turnID) { ProgressView() }
                        if targetUnavailable { Text("report.unavailable").foregroundStyle(.secondary) }
                        if reader.failedActivities.contains(turnID) {
                            Text("report.unavailable")
                            Button("reading.retry") { Task { await reader.reloadActivities(turnID) } }
                        }
                        if reader.activities[turnID]?["has_more"] == .bool(true) {
                            Button("reading.olderActivities") { Task { await reader.loadOlderActivities(turnID) } }
                                .disabled(reader.loadingActivities.contains(turnID))
                        }
                        if reader.activities[turnID] != nil &&
                            !ActivityPresentation.items(reader.activities[turnID]).contains(where: { $0.kind == "assistant" }) {
                            Text("report.noResponses")
                        }
                    }
                }
            }
            .navigationTitle("report.title").navigationBarTitleDisplayMode(.inline)
            .toolbar {
                Button("files.dismiss") { dismiss() }
                    .disabled(draft?.isSubmitting == true)
                    .accessibilityIdentifier("report.dismiss")
            }
            .interactiveDismissDisabled(draft?.isSubmitting == true)
            .task { if reader.contentReportScope == scope { await reader.loadActivities(turnID) } }
            .alert("report.confirmTitle", isPresented: $confirming) {
                Button("action.cancel", role: .cancel) {}
                Button("report.submit") {
                    guard reader.contentReportScope == scope, let draft else { return }
                    Task { await draft.submit { try await reader.createContentReport($0, scope: $1) } }
                }.accessibilityIdentifier("report.confirm")
            } message: { Text("report.confirmMessage") }
        }
    }

    private func select(_ activity: ReadingActivity) async {
        targetUnavailable = false
        defer { selectedActivityID = nil }
        await reader.loadActivityDetail(turnID: turnID, activityID: activity.id)
        guard reader.contentReportScope == scope,
              let detail = reader.activityDetails[activity.id],
              detail["agent_id"] == .string(scope.agentID),
              let target = ContentReportTarget(turnID: turnID, activity: activity, detail: detail) else {
            targetUnavailable = true
            return
        }
        draft = reader.contentReportDraft(target: target, scope: scope)
        if draft == nil { targetUnavailable = true }
    }

    @ViewBuilder private func reportForm(_ draft: ContentReportDraft) -> some View {
        @Bindable var draft = draft
        Section("report.response") {
            Text(verbatim: String(draft.target.text.prefix(8_000))).textSelection(.enabled)
            if draft.target.text.count > 8_000 { Text("report.previewTruncated").font(.caption) }
        }
        Section("report.destination") {
            Text(verbatim: scope.partition.api).font(.caption).textSelection(.enabled)
            Text("report.disclosure").font(.caption)
            Text("report.noSecrets").font(.caption)
        }
        if let receipt = draft.receipt {
            Section {
                Label("report.accepted", systemImage: "checkmark.circle")
                    .accessibilityIdentifier("report.accepted")
                Text("report.notModerated").font(.caption)
                Text(verbatim: receipt.reportID).font(.caption).textSelection(.enabled)
            }
        } else {
            Section("report.reason") {
                Picker("report.reason", selection: $draft.category) {
                    Text("report.chooseReason").tag(nil as ContentReportCategory?)
                    ForEach(ContentReportCategory.allCases) { category in
                        Text(LocalizedStringKey(category.title)).tag(Optional(category))
                    }
                }.disabled(draft.request != nil)
                    .accessibilityIdentifier("report.category")
                TextEditor(text: $draft.explanation).frame(minHeight: 90)
                    .disabled(draft.request != nil)
                    .accessibilityIdentifier("report.explanation")
                Text("report.descriptionLimit \(draft.explanation.unicodeScalars.count)")
                    .font(.caption).foregroundStyle(draft.explanation.unicodeScalars.count > 2_000 ? .red : .secondary)
            }
            Section {
                if let key = draft.errorKey {
                    Text(LocalizedStringKey(key)).accessibilityIdentifier("report.error")
                    Text("report.retryNotice").font(.caption)
                }
                if draft.isSubmitting { ProgressView("report.submitting") }
                Button(draft.request == nil ? "report.submit" : "report.retry") { confirming = true }
                    .disabled(!draft.canSubmit || reader.contentReportScope != scope)
                    .accessibilityIdentifier("report.submit")
            }
        }
    }
}
