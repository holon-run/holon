import SwiftUI
import HolonClient

struct TaskResultLabel: View {
    let result: TaskResultInput
    var showReason = false
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: result.isFailure ? "exclamationmark.circle" : result.status == "completed" ? "checkmark.circle" : "clock")
                if let title = result.title { Text(verbatim: title).lineLimit(1) }
                else if result.responseMessageID == nil { Text("reading.taskResult") }
                Text(LocalizedStringKey(result.statusKey)).lineLimit(1)
            }
            if showReason, let reason = result.reason, !reason.isEmpty { Text(verbatim: reason).lineLimit(1) }
        }
        .font(.caption).foregroundStyle(result.isFailure ? Color.red : Color.secondary)
    }
}

struct TaskResultRowView: View {
    let result: TaskResultInput
    let agentID: String?
    var fallbackTime: String? = nil
    @Environment(\.holonOpenTask) private var openTask

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TaskResultLabel(result: result)
            if let date = (result.createdAt ?? fallbackTime).flatMap(ReadingPresentation.date) {
                Text(date, format: .dateTime.month(.abbreviated).day().hour().minute()).font(.caption2).foregroundStyle(.secondary)
            }
            if result.responseMessageID == nil, !result.preview.isEmpty {
                Text(verbatim: result.displayPreview).font(.callout).textSelection(.enabled)
            }
            if agentID != nil {
                Button { openTask?(result.taskID) } label: {
                    Label(LocalizedStringKey(result.responseMessageID == nil ? "work.output" : "reading.originalReply"), systemImage: "arrow.up.right")
                        .font(.caption).frame(minHeight: 44)
                }
                .disabled(openTask == nil)
                .accessibilityIdentifier("taskResult.source." + result.id)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("taskResult." + result.id)
    }
}

struct PendingEventsView: View {
    let inputs: [JSONValue]
    let agentID: String?
    @State private var expanded = false

    var body: some View {
        if !inputs.isEmpty {
            DisclosureGroup(isExpanded: $expanded) {
                ForEach(inputs, id: \.pendingMessageID) { input in
                    if let result = TaskResultInput(input) {
                        TaskResultRowView(result: result, agentID: agentID)
                    } else {
                        DisclosureGroup {
                            Text(verbatim: input["preview"]?.readingString ?? "").font(.callout).textSelection(.enabled)
                        } label: {
                            Text(verbatim: input["preview"]?.readingString ?? "").font(.caption).lineLimit(2)
                        }
                    }
                    HStack {
                        Text(LocalizedStringKey(input["state"] == .string("assigning") ? "reading.assigning" : "reading.queued"))
                        if let date = input["created_at"]?.readingString.flatMap(ReadingPresentation.date) {
                            Text(date, format: .dateTime.month(.abbreviated).day().hour().minute())
                        }
                    }.font(.caption2).foregroundStyle(.secondary)
                }
            } label: {
                HStack { Text("reading.pendingEvents"); Text(verbatim: " · \(inputs.count)") }
                    .font(.caption).foregroundStyle(.secondary).frame(minHeight: 44)
            }
            .accessibilityIdentifier("conversation.pendingEvents")
        }
    }
}

private extension JSONValue {
    var pendingMessageID: String { self["message_id"]?.readingString ?? "" }
}
