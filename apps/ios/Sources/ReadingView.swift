import SwiftUI
import HolonClient

struct ReadingView: View {
    @Bindable var reader: ReadingCoordinator
    let connection: ConnectionCoordinator
    @State private var query = ""
    @State private var filter: AgentListFilter = .all
    @State private var window = 80
    @State private var visibleAgents: Set<String> = []

    private var matching: [ReadingAgent] {
        AgentSummaryPresentation.sorted(reader.agents, query: query, filter: filter)
    }
    /// A connection attempt for the selected network is still verifying identity online.
    private var isConnecting: Bool { connection.status == .connecting }
    /// The roster is loading from either the reading bootstrap or the connection attempt itself.
    private var rosterLoading: Bool {
        reader.status == .syncing || (reader.status == .offline && isConnecting)
    }

    var body: some View {
            List {
                Section {
                    // Keep one list row while connection state changes offscreen.
                    VStack(alignment: .leading, spacing: 12) {
                        NavigationLink(value: AppRoute.connections) {
                            HStack {
                                Image(systemName: "network").foregroundStyle(.secondary)
                                Text(verbatim: connection.selectedProfile?.name ?? "")
                                Spacer()
                                if let key = ReadingPresentation.connectionStatusKey(
                                    status: reader.status, hasContent: true,
                                    isCached: reader.isShowingCachedContent, isRefreshing: reader.isRefreshing,
                                    connecting: isConnecting
                                ) {
                                    Text(LocalizedStringKey(key))
                                        .font(.caption).foregroundStyle(.secondary)
                                        .accessibilityIdentifier("agents.connectionStatus")
                                }
                            }
                        }
                        .accessibilityIdentifier("connection.manage")
                        if rosterLoading && reader.agents.isEmpty { ProgressView() }
                        if reader.agents.isEmpty {
                            if reader.status == .live {
                                Text("reading.noAgentsConnected")
                                Button("connection.refresh") { Task { await reader.refresh() } }
                                    .buttonStyle(.borderless)
                            } else if reader.status == .offline && !isConnecting {
                                Text("reading.cache.noAgents")
                            } else if !rosterLoading {
                                Text("reading.emptyAgents")
                            }
                        }
                        if ReadingPresentation.showsConnectionRetry(status: reader.status,
                                                                    isRefreshing: reader.isRefreshing,
                                                                    connecting: isConnecting) {
                            Button("reading.retry") { Task { await reader.refresh() } }
                                .buttonStyle(.borderless)
                                .accessibilityIdentifier("agents.retry")
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
                                    HStack(alignment: .firstTextBaseline, spacing: 5) {
                                        if AgentSummaryPresentation.showsOperatorPreview(agent) {
                                            Text("reading.inputPreview").font(.caption).foregroundStyle(.secondary)
                                        }
                                        Text(verbatim: preview).lineLimit(2).foregroundStyle(.secondary)
                                    }
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
            .navigationBarTitleDisplayMode(.inline)
            .refreshable { await reader.refresh() }
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    NavigationLink(value: AppRoute.settings) {
                        Label("settings.title", systemImage: "gearshape")
                    }.accessibilityIdentifier("settings.open")
                }
                ToolbarItem(placement: .secondaryAction) {
                    Menu {
                        Picker("agents.filter", selection: $filter) {
                            ForEach(AgentListFilter.allCases, id: \.self) { option in
                                Text(LocalizedStringKey("agents.filter." + option.rawValue)).tag(option)
                            }
                        }
                    } label: {
                        Label("agents.filter", systemImage: filter == .all ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
                    }.accessibilityIdentifier("agents.filter")
                }
            }
            .onChange(of: query) { _, _ in window = 80 }
            .onChange(of: filter) { _, _ in window = 80 }
            .task(id: "\(reader.rosterRevision)|\(reader.status.rawValue)|\(visibleAgents.sorted().joined(separator: "|"))") {
                do { try await Task.sleep(for: .milliseconds(150)) } catch { return }
                await reader.loadVisiblePreviews(agentIDs: visibleAgents.sorted())
            }
    }
}

struct ConversationReadingView: View {
    private enum ScrollRequest: Hashable { case top(String), latest }
    @Bindable var reader: ReadingCoordinator
    let sender: SendingCoordinator?
    var apiBaseURL: URL?
    var openReference: (String) -> Void
    var openWork: (String) -> Void
    var openTask: (String) -> Void
    @State private var nearBottom = true
    @State private var follow = ConversationFollowState()
    @State private var newContent = false
    @State private var initializedAgent: String?
    @State private var readThroughToConfirm: ReadingReadConfirmation?
    @State private var historyEndTurnID: String?
    @State private var visibleTurnIDs: [String] = []
    @State private var userScrolling = false
    @State private var scrollRequest: ScrollRequest?
    @State private var scrollPosition = ScrollPosition(idType: String.self)
    @State private var showPrivacyInfo = false

    private var turnIDs: [String] { turns.map(\.id) }
    private var turnRange: Range<Int> { ConversationTurnWindow.range(ids: turnIDs, endingAt: historyEndTurnID) }
    private var visibleTurns: ArraySlice<ReadingTurnPresentation> { turns[turnRange] }

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
        Group {
        if reader.snapshot == nil || initializedAgent == reader.snapshot?.agentID {
        ScrollView {
            // The explicit turn window is bounded to20. Use its actual heights:
            // lazy offscreen estimates can loop while scrolling rich results.
            VStack(alignment: .leading, spacing: 16) {
                if turnRange.lowerBound > 0 || reader.canLoadHistory {
                    Button("reading.history") {
                        follow.reviewHistory()
                        Task {
                            let agent = reader.selectedAgentID
                            let epoch = reader.snapshot?.eventLogEpoch
                            if turnRange.lowerBound > 0 {
                                historyEndTurnID = ConversationTurnWindow.olderEnd(ids: turnIDs, current: turnRange)
                            } else {
                                let previousFirst = turnIDs.first
                                await reader.loadHistory()
                                guard reader.selectedAgentID == agent, reader.snapshot?.eventLogEpoch == epoch else { return }
                                if let previousFirst, let index = turnIDs.firstIndex(of: previousFirst), index > 0 {
                                    historyEndTurnID = turnIDs[min(turnIDs.count - 1, index + 4)]
                                }
                            }
                            if let first = visibleTurns.first { scrollRequest = .top(first.id) }
                        }
                    }
                        .disabled(reader.isLoadingHistory || (turnRange.lowerBound == 0 && reader.status != .live))
                        .accessibilityIdentifier("conversation.older")
                }
                if reader.isLoadingHistory { ProgressView() }
                if reader.status == .offline && reader.snapshot == nil {
                    Text("reading.cache.noConversation").foregroundStyle(.secondary)
                        .accessibilityIdentifier("conversation.notCached")
                }
                if reader.snapshot != nil && turns.isEmpty && reader.snapshot?.raw["pending_inputs"].viewArray.isEmpty == true {
                    Text("reading.emptyConversation")
                }
                ForEach(visibleTurns) { turn in
                    ReadingTurnView(turn: turn, reader: reader)
                        .environment(\.holonOpenReference, reader.status == .live ? openReference : nil)
                        .environment(\.holonOpenWork, reader.status == .live ? openWork : nil)
                        .environment(\.holonOpenTask, reader.status == .live ? openTask : nil)
                        .id(turn.id)
                }
                if turnRange.upperBound < turns.count {
                    Button("reading.newerTurns") {
                        follow.reviewHistory()
                        let ids = turnIDs
                        let nextEnd = ConversationTurnWindow.newerEnd(ids: ids, current: turnRange)
                        let nextRange = ConversationTurnWindow.range(ids: ids, endingAt: nextEnd)
                        historyEndTurnID = nextEnd
                        if !nextRange.isEmpty { scrollRequest = .top(ids[nextRange.lowerBound]) }
                    }.accessibilityIdentifier("conversation.newer")
                }
                ForEach((reader.snapshot?.raw["pending_inputs"].viewArray ?? []).filter { $0["presentation_class"] == .string("operator") && TaskResultInput($0) == nil }, id: \.viewMessageID) { input in
                    ReadingInputView(input: input)
                        .environment(\.holonOpenReference, reader.status == .live ? openReference : nil)
                        .accessibilityIdentifier("pending." + input.viewMessageID)
                }
                PendingEventsView(inputs: (reader.snapshot?.raw["pending_inputs"].viewArray ?? []).filter { $0["presentation_class"] != .string("operator") || TaskResultInput($0) != nil }, agentID: reader.selectedAgentID)
                    .environment(\.holonOpenTask, reader.status == .live ? openTask : nil)
                if let sender {
                    LocalMessageView(sender: sender, canonicalIDs: LocalMessageProjection.canonicalIDs(reader.snapshot?.raw))
                        .environment(\.holonOpenReference, reader.status == .live ? openReference : nil)
                }
                Color.clear.frame(height: 1).id("conversation-tail")
            }
            .frame(width: max(0, geometry.size.width - 32), alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.vertical, 16)
            .scrollTargetLayout()
        }
        .scrollPosition($scrollPosition)
        // Initial layout must not override a saved historical ID with the tail.
        .defaultScrollAnchor(reader.readingPosition == nil ? .bottom : .top, for: .initialOffset)
        .onScrollTargetVisibilityChange(idType: String.self, threshold: 0.1) { ids in
            visibleTurnIDs = ids
        }
        .onScrollPhaseChange { _, phase in
            if phase == .interacting { userScrolling = true }
            if phase == .idle, userScrolling {
                userScrolling = false
                follow.userEndedScroll(nearBottom: nearBottom, newestWindow: historyEndTurnID == nil)
                if let first = turnIDs.first(where: { visibleTurnIDs.contains($0) }) {
                    reader.rememberPosition(turnID: first)
                }
            }
        }
        .scrollDismissesKeyboard(.interactively)
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentSize.height - geometry.visibleRect.maxY < 100
        } action: { _, value in
            nearBottom = value
            if value { newContent = false }
        }
        .task(id: scrollRequest) {
            guard let request = scrollRequest else { return }
            // Keep the ID/edge as native position state so the new window's
            // committed layout resolves it, rather than a one-shot proxy read.
            switch request {
            case .top(let id):
                if visibleTurns.contains(where: { $0.id == id }) {
                    scrollPosition.scrollTo(id: id, anchor: .top)
                    reader.rememberPosition(turnID: id)
                }
            case .latest: scrollPosition.scrollTo(edge: .bottom)
            }
            scrollRequest = nil
        }
        .onChange(of: latestContentKey) { _, _ in
            if follow.followsLatest, nearBottom, historyEndTurnID == nil { scrollRequest = .latest }
            else { newContent = true }
        }
        .onChange(of: reader.snapshot?.eventLogEpoch) { old, new in
            if let old, let new, old != new { follow.showLatest(); historyEndTurnID = nil; scrollRequest = .latest }
        }
        .overlay(alignment: .bottomTrailing) {
            if !nearBottom || historyEndTurnID != nil || !follow.followsLatest {
                Button {
                    follow.showLatest()
                    historyEndTurnID = nil; scrollRequest = .latest; newContent = false
                    if let latest = turnIDs.last { reader.rememberPosition(turnID: latest) }
                } label: {
                    Label(newContent ? "reading.newContent" : "reading.latest", systemImage: "arrow.down")
                        .font(.caption).padding(10).background(.regularMaterial, in: Capsule())
                }
                .accessibilityIdentifier("conversation.latest").padding(12)
            }
        }
        } else {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        }
        .task(id: reader.snapshot?.agentID) {
            guard let agent = reader.snapshot?.agentID else { initializedAgent = nil; return }
            guard initializedAgent != agent else { return }
            // Mount the snapshot's window only after its initial native target
            // is ready. A post-layout scroll cannot define the first layout.
            if let saved = reader.readingPosition, turnIDs.contains(saved) {
                follow.reviewHistory()
                historyEndTurnID = ConversationTurnWindow.restoringEnd(ids: turnIDs, turnID: saved)
                scrollPosition = ScrollPosition(id: saved, anchor: .top)
            } else {
                follow.showLatest()
                historyEndTurnID = nil
                scrollPosition = ScrollPosition(idType: String.self)
                scrollPosition.scrollTo(edge: .bottom)
            }
            scrollRequest = nil
            visibleTurnIDs = []
            userScrolling = false
            newContent = false
            initializedAgent = agent
        }
        .navigationTitle(Text(verbatim: reader.agents.first { $0.id == reader.selectedAgentID }?.name ?? "Holon"))
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .top, spacing: 0) {
            ConversationConnectionStatus(status: reader.status, hasSnapshot: reader.snapshot != nil,
                                         isCached: reader.isShowingCachedContent, isRefreshing: reader.isRefreshing) {
                Task { await reader.refresh() }
            }
        }
        .safeAreaInset(edge: .bottom) {
            VStack(spacing: 0) {
                if reader.status != .live, sender?.canPersistDraft == true {
                    // Offline keeps the composer available: drafts are saved
                    // locally and delivery fails with an explicit per-message
                    // retry. A disconnected sender (session expired) cannot
                    // persist input, so the promise only shows while true.
                    Text("reading.cache.readOnly").font(.caption).foregroundStyle(.secondary)
                        .padding(.top, 6)
                        .accessibilityIdentifier("conversation.readOnly")
                }
                if let sender {
                    SendingView(sender: sender, currentRunID: currentRunID,
                                commonModels: reader.agents.compactMap(\.effectiveModel))
                } else {
                    Text("status.storageError").font(.caption).padding()
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    if reader.status == .live, let agent = reader.selectedAgentID {
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
                    Button { showPrivacyInfo = true } label: { Label("privacy.aiInfo", systemImage: "hand.raised") }
                        .accessibilityIdentifier("conversation.privacyInfo")
                } label: { Image(systemName: "ellipsis.circle").accessibilityLabel(Text("conversation.more")) }
                .accessibilityIdentifier("conversation.more")
            }
        }
        .sheet(isPresented: $showPrivacyInfo) { PrivacyInfoSheet(apiBaseURL: apiBaseURL) }
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
    @Environment(\.holonOpenTask) private var openTask
    @State private var reportScope: ContentReportScope?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                if let date = turn.raw["started_at"].viewString.flatMap(ReadingPresentation.date) {
                    Text(date, format: .dateTime.month(.abbreviated).day().hour().minute())
                        .accessibilityIdentifier("conversation.time." + turn.id)
                }
                Spacer()
                if let status = ReadingPresentation.turnStatus(turn.raw) { Text(LocalizedStringKey(status)) }
            }
            .font(.caption2).foregroundStyle(.secondary)
            ForEach(turn.raw["inputs"].viewArray, id: \.viewMessageID) { input in
                ReadingInputView(input: input, fallbackClass: turn.raw["presentation_class"].viewString)
            }
            DisclosureGroup(isExpanded: $showActivities) {
                Button("reading.fullProcess", systemImage: "arrow.up.left.and.arrow.down.right") { fullActivities = true }
                    .font(.caption).frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                    .accessibilityIdentifier("activities.full." + turn.id)
                TurnActivityView(turnID: turn.id, reader: reader, inputs: turn.raw["inputs"].viewArray, fallbackTime: turn.raw["started_at"].viewString)
            } label: {
                Group {
                    if let result = TaskResultInput.header(turn.raw["inputs"].viewArray) { TaskResultLabel(result: result, showReason: true) }
                    else { Text("reading.activities").font(.caption).foregroundStyle(.secondary) }
                }
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                .accessibilityIdentifier("activities." + turn.id)
            }
            .task(id: "\(showActivities)|\(reader.activityReadKey)") {
                if showActivities, reader.status == .live { await reader.loadActivities(turn.id) }
            }
            .sheet(isPresented: $fullActivities) {
                NavigationStack {
                    ScrollView { TurnActivityView(turnID: turn.id, reader: reader, inputs: turn.raw["inputs"].viewArray, fallbackTime: turn.raw["started_at"].viewString).padding() }
                        .environment(\.holonOpenTask, { taskID in fullActivities = false; openTask?(taskID) })
                        .accessibilityIdentifier("activities.fullReader")
                        .task(id: reader.activityReadKey) {
                            if reader.status == .live { await reader.loadActivities(turn.id) }
                        }
                        .navigationTitle("reading.activities").navigationBarTitleDisplayMode(.inline)
                        .toolbar { Button("files.dismiss") { fullActivities = false } }
                }
            }
            .sheet(item: $reportScope) { scope in
                ContentReportView(turnID: turn.id, scope: scope, reader: reader)
            }
            ForEach(turn.raw["brief_ids"].viewArray.compactMap(\.viewString), id: \.self) { briefID in
                ReadingBriefView(briefID: briefID, reader: reader, inputs: turn.raw["inputs"].viewArray,
                                 onReport: { reportScope = reader.contentReportScope })
            }
            Divider()
        }
        .contextMenu {
            if reader.contentReportScope != nil {
                Button("report.title", systemImage: "flag") { reportScope = reader.contentReportScope }
                    .accessibilityIdentifier("report.open." + turn.id)
            }
        }
    }
}

struct ReadingInputView: View {
    let input: JSONValue
    var fallbackClass: String? = nil

    var body: some View {
        if (input["presentation_class"].viewString ?? fallbackClass) == "operator" {
            OperatorMessageBubble {
                VStack(alignment: .leading, spacing: 6) {
                    OperatorMessageText(text: ReadingPresentation.operatorText(input))
                    if let actor = input["actor_display_name"].viewString?.trimmingCharacters(in: .whitespacesAndNewlines), !actor.isEmpty {
                        Text(verbatim: actor).font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
            .accessibilityIdentifier("operator." + input.viewMessageID)
        }
    }
}

/// Connection state is chrome, not an invented entry in the conversation.
struct ConversationConnectionStatus: View {
    let status: ReadingStatus
    let hasSnapshot: Bool
    var isCached = false
    var isRefreshing = false
    var retry: () -> Void
    var body: some View {
        if let key = ReadingPresentation.connectionStatusKey(
            status: status, hasContent: hasSnapshot, isCached: isCached, isRefreshing: isRefreshing
        ) {
            HStack(spacing: 8) {
                if isRefreshing || status == .syncing || (status == .live && !hasSnapshot) {
                    ProgressView().controlSize(.mini)
                } else { Image(systemName: isCached && status == .live ? "internaldrive" : "wifi.exclamationmark") }
                Text(LocalizedStringKey(key))
                    .font(.caption).foregroundStyle(.secondary)
                Spacer(minLength: 0)
                if ReadingPresentation.showsConnectionRetry(status: status, isRefreshing: isRefreshing) {
                    Button("reading.retry", action: retry).font(.caption).frame(minHeight: 44)
                        .accessibilityIdentifier("conversation.retry")
                }
            }
            .padding(.horizontal, 16).padding(.vertical, 8)
            .background(.bar)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("conversation.connectionStatus")
        }
    }
}

struct OperatorMessageBubble<Content: View>: View {
    @ViewBuilder var content: () -> Content
    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            Spacer(minLength: 24)
            content().padding(.horizontal, 13).padding(.vertical, 10)
                .background(Color(uiColor: .secondarySystemBackground),
                            in: UnevenRoundedRectangle(topLeadingRadius: 16, bottomLeadingRadius: 16,
                                                       bottomTrailingRadius: 4, topTrailingRadius: 16))
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("reading.operator"))
    }
}

private struct ReadingBriefView: View {
    let briefID: String
    @Bindable var reader: ReadingCoordinator
    var inputs: [JSONValue] = []
    var onReport: (() -> Void)? = nil
    @State private var isVisible = false
    @Environment(\.holonOpenReference) private var openReference
    @Environment(\.holonOpenWork) private var openWork

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let brief = reader.briefs[briefID] {
                if !TaskResultInput.isRuntimeBrief(brief, inputs: inputs) {
                    RichTextContent(text: BriefPresentation.text(brief), openReference: openReference,
                                    onReport: reader.contentReportScope == nil ? nil : onReport)
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
                }
            } else {
                BriefPlaceholderView(briefID: briefID, state: BriefPlaceholderState(
                    isLoading: reader.loadingBriefs.contains(briefID),
                    hasFailed: reader.failedBriefs.contains(briefID),
                    canLoad: reader.status == .live
                )) { Task { await reader.loadBrief(briefID) } }
            }
        }
        // Native accessibility can materialize offscreen lazy rows. Fetch/render only
        // visible results so a long history cannot exhaust the detail queue on arrival.
        .task(id: "\(isVisible)|\(reader.status.rawValue)") {
            if isVisible {
                await reader.loadBrief(briefID)
                guard !Task.isCancelled, isVisible else { return }
                updateReadVisibility()
            }
        }
        .onChange(of: reader.briefs[briefID]) { _, _ in updateReadVisibility() }
        .onScrollVisibilityChange(threshold: 0.5) { visible in
            isVisible = visible
            updateReadVisibility()
        }
        .onDisappear {
            isVisible = false
            reader.setBriefVisible(briefID, visible: false)
        }
    }

    private func updateReadVisibility() {
        reader.setBriefVisible(briefID, visible: isVisible && !(reader.briefs[briefID].map { TaskResultInput.isRuntimeBrief($0, inputs: inputs) } ?? false))
        if isVisible { reader.markVisibleBriefsReadAutomatically() }
    }
}

enum ReadingPresentation {
    static func connectionStatusKey(status: ReadingStatus, hasContent: Bool,
                                    isCached: Bool, isRefreshing: Bool,
                                    connecting: Bool = false) -> String? {
        if status == .permissionDenied || status == .sessionExpired || status == .incompatible {
            return "reading.status." + status.rawValue
        }
        // A pending connection attempt keeps verifying the target network in the background.
        if isRefreshing || status == .syncing || (connecting && status == .offline) {
            return isCached ? "reading.cache.refreshing" : "reading.status.syncing"
        }
        if status == .offline {
            return isCached ? "reading.cache.offline" : "reading.status.offline"
        }
        if status == .live {
            if !hasContent { return "reading.loadingConversation" }
            return isCached ? "reading.cache.saved" : nil
        }
        return "reading.status." + status.rawValue
    }

    static func showsConnectionRetry(status: ReadingStatus, isRefreshing: Bool,
                                     connecting: Bool = false) -> Bool {
        !(connecting || isRefreshing) && (status == .offline || status == .incompatible)
    }

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
        OperatorInputPresentation.text(input["preview"].viewString ?? "")
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
