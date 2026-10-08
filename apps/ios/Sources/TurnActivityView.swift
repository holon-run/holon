import SwiftUI
import HolonClient

extension EnvironmentValues {
    @Entry var holonOpenReference: ((String) -> Void)? = nil
    @Entry var holonOpenWork: ((String) -> Void)? = nil
}

struct TurnActivityView: View {
    let turnID: String
    @Bindable var reader: ReadingCoordinator

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            let page = reader.activities[turnID]
            if page?["has_more"] == .bool(true) {
                Button("reading.olderActivities") { Task { await reader.loadOlderActivities(turnID) } }
                    .disabled(reader.loadingActivities.contains(turnID))
                    .accessibilityIdentifier("activities.older")
            }
            if page?["client_window_trimmed"] == .bool(true) {
                Text("reading.activityWindow").font(.caption).foregroundStyle(.secondary)
                Button("reading.newestActivities") { Task { await reader.reloadActivities(turnID) } }
            }
            if page?["coverage"]?["kind"] != nil && page?["coverage"]?["kind"] != .string("complete") {
                Text("reading.partialActivities").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(ActivityPresentation.items(page).filter { $0.kind != "operator" }) { activity in
                ActivityRowView(activity: activity, turnID: turnID, reader: reader)
            }
            if reader.loadingActivities.contains(turnID) { ProgressView() }
            if reader.failedActivities.contains(turnID) || page == nil && !reader.loadingActivities.contains(turnID) {
                Text("reading.detailUnavailable").font(.caption).foregroundStyle(.secondary)
                Button("reading.reloadActivities") { Task { await reader.reloadActivities(turnID) } }
            }
        }
        .font(.callout)
        .padding(.vertical, 8)
    }
}

private struct ActivityRowView: View {
    let activity: ReadingActivity
    let turnID: String
    @Bindable var reader: ReadingCoordinator
    @Environment(\.holonOpenReference) private var openReference
    @State private var expanded = false

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol).font(.caption).foregroundStyle(.secondary).frame(width: 16)
            VStack(alignment: .leading, spacing: 8) {
                if activity.kind == "assistant" {
                    Text("Assistant").font(.caption).foregroundStyle(.secondary)
                    let detail = reader.activityDetails[activity.id]
                    let text = expanded && detail != nil ? ActivityPresentation.assistantText(detail?["data"] ?? .null) :
                        ActivityPresentation.assistantText(.string(activity.summary))
                    if text.isEmpty { Text("reading.noAssistantText").foregroundStyle(.secondary) }
                    else { RichTextContent(text: text, openReference: openReference) }
                    DisclosureGroup("reading.fullAssistant", isExpanded: $expanded) {
                        detailState
                        rawDetails
                    }.font(.caption)
                } else if activity.kind == "tool" {
                    DisclosureGroup(isExpanded: $expanded) {
                        detailState
                        if let detail = reader.activityDetails[activity.id] {
                            if let duration = detail["duration_ms"]?.readingInteger {
                                Text("reading.duration \(duration)").font(.caption).foregroundStyle(.secondary)
                            }
                            ForEach(Array(ActivityPresentation.toolBlocks(detail).enumerated()), id: \.offset) { _, block in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(LocalizedStringKey(block.key)).font(.caption).foregroundStyle(.secondary)
                                    Text(verbatim: block.text).font(.callout.monospaced()).textSelection(.enabled)
                                }
                            }
                            rawDetails
                        }
                    } label: {
                        Text(verbatim: activity.summary).foregroundStyle(.secondary)
                    }
                } else {
                    Text(LocalizedStringKey(activity.kind == "wait" ? "reading.wait" :
                        activity.kind == "error" ? "reading.error" : "reading.unknownActivity"))
                        .font(.caption).foregroundStyle(.secondary)
                    Text(verbatim: activity.summary).textSelection(.enabled)
                    rawDetails
                }
            }
        }
        .accessibilityIdentifier("activity." + activity.id)
        .task(id: "\(expanded)|\(activity.revision)|\(reader.activityCacheRevision)|\(reader.activities[turnID]?["detail_revision"]?.readingInteger ?? -1)") {
            if expanded { await reader.loadActivityDetail(turnID: turnID, activityID: activity.id) }
        }
    }

    @ViewBuilder private var detailState: some View {
        if reader.loadingActivities.contains(activity.id) { ProgressView() }
        else if reader.activityDetails[activity.id] == nil {
            Text("reading.detailUnavailable").foregroundStyle(.secondary)
            Button("reading.retry") { Task { await reader.loadActivityDetail(turnID: turnID, activityID: activity.id) } }
        }
    }

    private var rawDetails: some View {
        DisclosureGroup("reading.rawRecord") {
            Text(verbatim: ActivityPresentation.display(reader.activityDetails[activity.id] ?? activity.raw))
                .font(.caption.monospaced()).textSelection(.enabled)
        }.font(.caption)
    }

    private var symbol: String {
        switch activity.kind {
        case "tool": "wrench.and.screwdriver"
        case "assistant": "text.bubble"
        case "error": "exclamationmark.circle"
        default: "clock"
        }
    }
}
