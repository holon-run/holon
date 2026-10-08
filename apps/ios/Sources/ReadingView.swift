import SwiftUI
import HolonClient

struct ReadingView: View {
    @Bindable var reader: ReadingCoordinator
    let connection: ConnectionCoordinator
    @State private var query = ""
    @State private var needsReply = false
    @State private var window = 80
    @State private var visibleAgents: Set<String> = []

    private var matching: [ReadingAgent] {
        AgentSummaryPresentation.sorted(reader.agents, query: query, needsReply: needsReply)
    }

    var body: some View {
            List {
                Section {
                    NavigationLink(value: AppRoute.connections) {
                        HStack {
                            Image(systemName: "network").foregroundStyle(.secondary)
                            Text(verbatim: connection.selectedProfile?.name ?? "")
                            Spacer()
                            if reader.status != .live {
                                Text(LocalizedStringKey("reading.status." + reader.status.rawValue))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    .accessibilityIdentifier("connection.manage")
                    if reader.status == .syncing && reader.agents.isEmpty { ProgressView() }
                    if reader.agents.isEmpty {
                        if reader.status == .live {
                            Text("reading.noAgentsConnected")
                            Button("connection.refresh") { Task { await reader.refresh() } }
                        } else if reader.status != .syncing {
                            Text("reading.emptyAgents")
                        }
                    }
                }
                Section("reading.recent") {
                    ForEach(Array(matching.prefix(window))) { agent in
                        NavigationLink(value: AppRoute.conversation(agent.id)) {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack {
                                    Text(verbatim: agent.name).font(.headline).foregroundStyle(.primary)
                                    Spacer()
                                    if let date = AgentSummaryPresentation.activityDate(agent) {
                                        Text(date, style: .relative).font(.caption2).foregroundStyle(.secondary)
                                    }
                                    if let unread = agent.unreadCount, unread > 0 {
                                        Text("reading.unread \(unread)").font(.caption2).foregroundStyle(.blue)
                                    }
                                }
                                let preview = AgentSummaryPresentation.preview(agent)
                                if !preview.isEmpty {
                                    Text(verbatim: preview).lineLimit(2).foregroundStyle(.secondary)
                                }
                                if AgentSummaryPresentation.needsReply(agent) {
                                    Label("agents.needsReply", systemImage: "bubble.left")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            .padding(.vertical, 4)
                        }
                        .accessibilityIdentifier("agent." + agent.id)
                        .onAppear { visibleAgents.insert(agent.id) }
                        .onDisappear { visibleAgents.remove(agent.id) }
                    }
                    if matching.count > window {
                        Button("agents.more") { window += 80 }
                    } else if matching.isEmpty && !reader.agents.isEmpty {
                        Text("agents.noMatches").foregroundStyle(.secondary)
                    }
                }
            }
            .listStyle(.plain)
            .searchable(text: $query, prompt: Text("agents.search"))
            .navigationTitle("agents.title")
            .refreshable { await reader.refresh() }
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    NavigationLink(value: AppRoute.settings) {
                        Label("settings.title", systemImage: "gearshape")
                    }.accessibilityIdentifier("settings.open")
                }
                ToolbarItem(placement: .secondaryAction) {
                    Toggle("agents.needsReply", isOn: $needsReply)
                }
            }
            .onChange(of: query) { _, _ in window = 80 }
            .task(id: "\(reader.rosterRevision)|\(reader.status.rawValue)|\(visibleAgents.sorted().joined(separator: "|"))") {
                do { try await Task.sleep(for: .milliseconds(150)) } catch { return }
                await reader.loadVisiblePreviews(agentIDs: visibleAgents.sorted())
            }
    }
}

struct ConversationReadingView: View {
    @Bindable var reader: ReadingCoordinator
    let sender: SendingCoordinator?
    var openReference: (String) -> Void
    var openWork: (String) -> Void
    @State private var position = ScrollPosition(idType: String.self, edge: .bottom)
    @State private var nearBottom = true
    @State private var newContent = false
    @State private var initializedAgent: String?
    @State private var readThroughToConfirm: ReadingReadConfirmation?

    private var turns: [ReadingTurnPresentation] {
        guard let raw = reader.snapshot?.raw else { return [] }
        var byID: [String: ReadingTurnPresentation] = [:]
        for turn in raw["turns"].viewArray + raw["active_turns"].viewArray {
            guard let item = ReadingTurnPresentation(raw: turn) else { continue }
            byID[item.id] = item
        }
        return byID.values.sorted {
            $0.index == $1.index ? $0.id < $1.id : $0.index < $1.index
        }
    }

    private var currentRunID: String? {
        guard reader.status == .live,
              let runID = reader.agents.first(where: { $0.id == reader.selectedAgentID })?.currentRunID,
              !runID.isEmpty else { return nil }
        return runID
    }

    var body: some View {
        GeometryReader { geometry in
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if reader.status != .live {
                    Text(LocalizedStringKey("reading.status." + reader.status.rawValue))
                        .font(.caption).foregroundStyle(.secondary)
                }
                if reader.canLoadHistory {
                    Button("reading.history") { Task { await reader.loadHistory() } }
                        .disabled(reader.isLoadingHistory)
                }
                if reader.isLoadingHistory { ProgressView() }
                if reader.snapshot == nil {
                    Text("reading.loadingConversation")
                } else if turns.isEmpty && reader.snapshot?.raw["pending_inputs"].viewArray.isEmpty == true {
                    Text("reading.emptyConversation")
                }
                LazyVStack(alignment: .leading, spacing: 20) {
                    ForEach(turns) { turn in
                        ReadingTurnView(turn: turn, reader: reader)
                            .environment(\.holonOpenReference, openReference)
                            .environment(\.holonOpenWork, openWork)
                            .id(turn.id)
                    }
                }
                .scrollTargetLayout()
                ForEach(reader.snapshot?.raw["pending_inputs"].viewArray ?? [], id: \.viewMessageID) { input in
                    ReadingInputView(input: input)
                        .accessibilityIdentifier("pending." + input.viewMessageID)
                }
                if let sender {
                    LocalMessageView(sender: sender, canonicalIDs: LocalMessageProjection.canonicalIDs(reader.snapshot?.raw))
                }
                Color.clear.frame(height: 1).id("conversation-tail")
            }
            .frame(width: max(0, geometry.size.width - 32), alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.vertical, 16)
        }
        .scrollPosition($position)
        .onScrollGeometryChange(for: Bool.self) { abs($0.contentOffset.x) > 0.5 } action: { _, displaced in
            // Native ID/edge positioning can retain a row's horizontal inset; this timeline is vertical only.
            if displaced { position.scrollTo(x: 0) }
        }
        .scrollDismissesKeyboard(.interactively)
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentSize.height - geometry.visibleRect.maxY < 100
        } action: { _, value in
            nearBottom = value
            if value { newContent = false }
        }
        .task(id: reader.snapshot?.agentID) {
            guard let agent = reader.snapshot?.agentID, initializedAgent != agent else { return }
            initializedAgent = agent
            if let saved = reader.readingPosition { position.scrollTo(id: saved, anchor: .top) }
            else { position.scrollTo(edge: .bottom) }
        }
        .onChange(of: position.viewID(type: String.self)) { _, value in
            if let value, turns.contains(where: { $0.id == value }) { reader.rememberPosition(turnID: value) }
        }
        .onChange(of: latestContentKey) { _, _ in
            if nearBottom { position.scrollTo(edge: .bottom) }
            else { newContent = true }
        }
        .overlay(alignment: .bottomTrailing) {
            if !nearBottom {
                Button {
                    position.scrollTo(edge: .bottom); newContent = false
                } label: {
                    Label(newContent ? "reading.newContent" : "reading.latest", systemImage: "arrow.down")
                        .font(.caption).padding(10).background(.regularMaterial, in: Capsule())
                }
                .accessibilityIdentifier("conversation.latest").padding(12)
            }
        }
        .navigationTitle(Text(verbatim: reader.agents.first { $0.id == reader.selectedAgentID }?.name ?? "Holon"))
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) {
            if let sender {
                SendingView(sender: sender, currentRunID: currentRunID, commonModels: reader.agents.compactMap(\.effectiveModel))
            } else {
                Text("status.storageError").font(.caption).padding()
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    if let agent = reader.selectedAgentID {
                        NavigationLink(value: AppRoute.work(agent)) { Label("work.title", systemImage: "checklist") }
                            .accessibilityIdentifier("conversation.work")
                        NavigationLink(value: AppRoute.files(agent)) { Label("files.title", systemImage: "folder") }
                            .accessibilityIdentifier("conversation.files")
                    }
                    Button("reading.refresh") { Task { await reader.refresh() } }
                    Button("reading.markRead") { readThroughToConfirm = reader.readConfirmationForVisibleBriefs }
                        .disabled(reader.readThroughForVisibleBriefs == nil)
                    NavigationLink(value: AppRoute.tools) { Label("settings.sharing", systemImage: "square.and.arrow.up") }
                    NavigationLink(value: AppRoute.diagnostics) { Label("diagnostics.title", systemImage: "stethoscope") }
                        .accessibilityIdentifier("diagnostics.open")
                } label: { Image(systemName: "ellipsis.circle").accessibilityLabel(Text("conversation.more")) }
                .accessibilityIdentifier("conversation.more")
            }
        }
        .confirmationDialog("reading.markRead", isPresented: Binding(
            get: { readThroughToConfirm != nil },
            set: { if !$0 { readThroughToConfirm = nil } }
        ), titleVisibility: .visible) {
            if let confirmation = readThroughToConfirm {
                Button("reading.confirmReadThrough") {
                    readThroughToConfirm = nil
                    Task { await reader.markRead(confirmation: confirmation) }
                }
            }
            Button("reading.cancel", role: .cancel) { readThroughToConfirm = nil }
        } message: {
            Text("reading.cumulativeReadNotice")
        }
        }
    }

    private var latestContentKey: String {
        let latest = turns.last
        let loadedBriefs = latest?.raw["brief_ids"].viewArray.compactMap(\.viewString).map { id in
            "\(id):\(reader.briefs[id].map { BriefPresentation.text($0).utf8.count } ?? -1)"
        }.joined(separator: "|") ?? ""
        return "\(latest?.id ?? "")|\(latest?.raw["revision"].viewJSON ?? "")|\(loadedBriefs)|\(reader.snapshot?.raw["pending_inputs"].viewJSON ?? "")|\(sender?.entries.count ?? 0)"
    }
}

private struct ReadingTurnPresentation: Identifiable {
    let id: String
    let index: Int64
    let raw: JSONValue

    init?(raw: JSONValue) {
        guard let id = raw["turn_id"].viewString, !id.isEmpty,
              case .integer(let index)? = raw["key"]?["turn_index"] else { return nil }
        self.id = id
        self.index = index
        self.raw = raw
    }
}

private struct ReadingTurnView: View {
    let turn: ReadingTurnPresentation
    @Bindable var reader: ReadingCoordinator
    @State private var showActivities = false
    @State private var fullActivities = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                if let date = turn.raw["started_at"].viewString.flatMap(ReadingPresentation.date) {
                    Text(date, format: .dateTime.month(.abbreviated).day().hour().minute())
                }
                Spacer()
                if let status = ReadingPresentation.turnStatus(turn.raw) { Text(LocalizedStringKey(status)) }
            }
            .font(.caption2).foregroundStyle(.secondary)
            ForEach(turn.raw["inputs"].viewArray, id: \.viewMessageID) { input in
                ReadingInputView(input: input, fallbackClass: turn.raw["presentation_class"].viewString)
            }
            ForEach(turn.raw["brief_ids"].viewArray.compactMap(\.viewString), id: \.self) { briefID in
                ReadingBriefView(briefID: briefID, reader: reader)
            }
            DisclosureGroup(isExpanded: $showActivities) {
                TurnActivityView(turnID: turn.id, reader: reader)
                Button("reading.fullProcess", systemImage: "arrow.up.left.and.arrow.down.right") { fullActivities = true }
                    .font(.caption)
            } label: {
                Text("reading.activities").font(.caption).foregroundStyle(.secondary)
            }
            .task(id: "\(showActivities)|\(reader.snapshot?.snapshotCursor ?? "")") {
                if showActivities { await reader.loadActivities(turn.id) }
            }
            .sheet(isPresented: $fullActivities) {
                NavigationStack {
                    ScrollView { TurnActivityView(turnID: turn.id, reader: reader).padding() }
                        .navigationTitle("reading.activities").navigationBarTitleDisplayMode(.inline)
                        .toolbar { Button("files.dismiss") { fullActivities = false } }
                }
            }
            Divider()
        }
    }
}

private struct ReadingInputView: View {
    let input: JSONValue
    var fallbackClass: String? = nil

    var body: some View {
        if (input["presentation_class"].viewString ?? fallbackClass) == "operator" {
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text("reading.operator").font(.caption).bold()
                    Text(verbatim: input["actor_display_name"].viewString ?? "")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Text(verbatim: ReadingPresentation.operatorText(input)).textSelection(.enabled)
            }
        }
    }
}

private struct ReadingBriefView: View {
    let briefID: String
    @Bindable var reader: ReadingCoordinator
    @State private var isVisible = false
    @Environment(\.holonOpenReference) private var openReference
    @Environment(\.holonOpenWork) private var openWork

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let brief = reader.briefs[briefID] {
                RichTextContent(text: BriefPresentation.text(brief), openReference: openReference)
                ForEach(Array(brief["attachments"].viewArray.enumerated()), id: \.offset) { _, attachment in
                    if let reference = ReadingPresentation.attachmentReference(attachment) {
                        Button { openReference?(reference) } label: {
                            Label(attachment["name"].viewString ?? "", systemImage: "doc")
                                .font(.callout).frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 6)
                        }.disabled(openReference == nil)
                    } else {
                        Label(attachment["name"].viewString ?? "", systemImage: "paperclip").font(.callout)
                    }
                }
                if let workID = brief["work_item_id"].viewString {
                    Button("work.details", systemImage: "checklist") { openWork?(workID) }.font(.caption)
                        .disabled(openWork == nil)
                }
            } else {
                Text("reading.detailUnavailable")
                Button("reading.retry") { Task { await reader.loadBrief(briefID) } }
            }
        }
        .task { await reader.loadBrief(briefID) }
        .onChange(of: reader.briefs[briefID]) { _, _ in updateReadVisibility() }
        .onScrollVisibilityChange(threshold: 0.5) { visible in
            isVisible = visible
            updateReadVisibility()
        }
        .onDisappear { reader.setBriefVisible(briefID, visible: false) }
    }

    private func updateReadVisibility() {
        reader.setBriefVisible(briefID, visible: isVisible)
    }
}

enum ReadingPresentation {
    static func date(_ value: String) -> Date? {
        let formatter = ISO8601DateFormatter(); formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: value) ?? ISO8601DateFormatter().date(from: value)
    }

    static func turnStatus(_ turn: JSONValue) -> String? {
        switch turn["attention"]?["kind"]?.readingString {
        case "waiting": return "reading.wait"
        case "failed": return "reading.failedTurn"
        case "interrupted": return "reading.interruptedTurn"
        default: break
        }
        if turn["execution"]?["kind"] == .string("active") { return "reading.activeTurn" }
        switch turn["execution"]?["outcome"]?.readingString {
        case "aborted", "interrupted": return "reading.interruptedTurn"
        case "provider_failed_needs_recovery", "baseline_over_budget": return "reading.failedTurn"
        default: return nil
        }
    }

    static func attachmentReference(_ attachment: JSONValue) -> String? {
        guard let uri = attachment["uri"]?.readingString, let url = URL(string: uri) else { return nil }
        if case .reference(let reference) = RichTextLink.classify(url) { return reference }
        return nil
    }
    /// Extract the text envelope without translating or rewriting agent content.
    static func operatorText(_ input: JSONValue) -> String {
        let preview = input["preview"].viewString ?? ""
        guard let data = preview.data(using: .utf8),
              let envelope = try? JSONDecoder().decode(JSONValue.self, from: data),
              envelope["type"].viewString == "text",
              let text = envelope["text"].viewString else { return preview }
        return text
    }
}

private extension Optional where Wrapped == JSONValue {
    var viewString: String? { self?.viewString }
    var viewArray: [JSONValue] { self?.viewArray ?? [] }
    var viewJSON: String { self?.viewJSON ?? "" }
}

private extension JSONValue {
    var viewString: String? {
        if case .string(let value) = self { return value }
        return nil
    }
    var viewArray: [JSONValue] {
        if case .array(let value) = self { return value }
        return []
    }
    var viewMessageID: String { self["message_id"].viewString ?? "" }
    var viewJSON: String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return (try? String(decoding: encoder.encode(self), as: UTF8.self)) ?? ""
    }
}
