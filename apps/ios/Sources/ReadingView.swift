import SwiftUI
import HolonClient

struct ReadingView: View {
    @Bindable var reader: ReadingCoordinator
    let connection: ConnectionCoordinator
    @State private var query = ""
    @State private var filter: AgentListFilter = .all
    @State private var window = 80
    @State private var visibleAgents: Set<String> = []
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    private var matching: [ReadingAgent] {
        AgentSummaryPresentation.sorted(reader.agents, query: query, filter: filter)
    }
    /// A connection attempt for the selected network is still verifying identity online.
    private var isConnecting: Bool { connection.status == .connecting }
    /// The roster is loading from either the reading bootstrap or the connection attempt itself.
    private var rosterLoading: Bool {
        reader.status == .syncing || (reader.status == .offline && isConnecting)
    }
    private var connectionStatusKey: String {
        ReadingPresentation.connectionStatusKey(
            status: reader.status, hasContent: true, isCached: reader.isShowingCachedContent,
            isRefreshing: reader.isInitialRosterRefreshing, connecting: isConnecting
        ) ?? "reading.status.live"
    }
    private var canRetryConnection: Bool {
        ReadingPresentation.showsConnectionRetry(status: reader.status,
                                                 isRefreshing: reader.isInitialRosterRefreshing,
                                                 connecting: isConnecting)
    }
    private var rosterBottomMargin: CGFloat? {
        // Leave enough scroll range to lift a tall row above the floating search bar.
        if #available(iOS 26, *), dynamicTypeSize.isAccessibilitySize { return 96 }
        return nil
    }
    private var connectionStatusLines: Int {
        if dynamicTypeSize.isAccessibilitySize { return 3 }
        return dynamicTypeSize >= .xLarge ? 2 : 1
    }
    private var usesStackedConnectionHeader: Bool {
        dynamicTypeSize >= .xLarge
    }
    private var connectionStatusDotColor: Color {
        switch reader.status {
        case .permissionDenied, .sessionExpired, .incompatible: .red
        case .offline: isConnecting ? .accentColor : .orange
        case .syncing: .accentColor
        case .live: (reader.isShowingCachedContent || reader.isInitialRosterRefreshing) ? .accentColor : .green
        case .disconnected: .secondary
        }
    }

    var body: some View {
            List {
                Section {
                    // Keep one list row while connection state changes offscreen.
                    VStack(alignment: .leading, spacing: 6) {
                        if usesStackedConnectionHeader {
                            HStack(spacing: 4) {
                                NavigationLink(value: AppRoute.connections) {
                                    let host = connection.selectedProfile?.name ?? ""
                                    Label {
                                        Text(verbatim: host)
                                            .font(.subheadline.weight(.medium))
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                    } icon: {
                                        Image(systemName: "network").foregroundStyle(Color.accentColor)
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .accessibilityLabel(Text(verbatim: host))
                                }
                                .accessibilityIdentifier("connection.manage")
                                Group {
                                    if canRetryConnection {
                                        Button {
                                            Task { await reader.refresh() }
                                        } label: {
                                            Image(systemName: "arrow.clockwise")
                                                .frame(width: 44, height: 44)
                                                .contentShape(Rectangle())
                                        }
                                        .buttonStyle(.borderless)
                                        .accessibilityLabel(Text("reading.retry"))
                                        .accessibilityIdentifier("agents.retry")
                                    } else {
                                        Color.clear.frame(width: 44, height: 44)
                                            .allowsHitTesting(false)
                                            .accessibilityHidden(true)
                                    }
                                }
                            }
                            Text(LocalizedStringKey(connectionStatusKey))
                                .font(.caption).foregroundStyle(.secondary)
                                .lineLimit(connectionStatusLines, reservesSpace: true)
                                .accessibilityIdentifier("agents.connectionStatus")
                        } else {
                            HStack(spacing: 8) {
                                NavigationLink(value: AppRoute.connections) {
                                    let host = connection.selectedProfile?.name ?? ""
                                    Label {
                                        Text(verbatim: host)
                                            .font(.subheadline.weight(.medium))
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                    } icon: {
                                        Image(systemName: "network").foregroundStyle(Color.accentColor)
                                    }
                                    .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                                    .accessibilityLabel(Text(verbatim: host))
                                }
                                .accessibilityIdentifier("connection.manage")
                                HStack(spacing: 5) {
                                    Circle()
                                        .fill(connectionStatusDotColor)
                                        .frame(width: 6, height: 6)
                                        .accessibilityHidden(true)
                                    Text(LocalizedStringKey(connectionStatusKey))
                                        .font(.caption.weight(.medium))
                                        .foregroundStyle(canRetryConnection ? Color.orange : Color.secondary)
                                        .lineLimit(1)
                                        .fixedSize(horizontal: true, vertical: false)
                                        .accessibilityIdentifier("agents.connectionStatus")
                                }
                                .padding(.horizontal, 8)
                                .padding(.vertical, 4)
                                .background(Color(uiColor: .secondarySystemFill), in: Capsule())
                                if canRetryConnection {
                                    Button {
                                        Task { await reader.refresh() }
                                    } label: {
                                        Image(systemName: "arrow.clockwise")
                                            .font(.subheadline.weight(.medium))
                                            .frame(width: 36, height: 36)
                                            .contentShape(Rectangle())
                                    }
                                    .buttonStyle(.borderless)
                                    .accessibilityLabel(Text("reading.retry"))
                                    .accessibilityIdentifier("agents.retry")
                                }
                            }
                            .frame(minHeight: 36)
                        }
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
                    }
                    .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                }
                Section {
                    ForEach(Array(matching.prefix(window))) { agent in
                        let presentation = AgentIdentityPresentation.derive(
                            agent, status: reader.status, isRosterCurrent: reader.hasDisplayableLiveRoster,
                            isCached: reader.rosterIsCached,
                            isRefreshing: reader.isInitialRosterRefreshing, connecting: isConnecting
                        )
                        NavigationLink(value: AppRoute.conversation(agent.id)) {
                            AgentSummaryRow(agent: agent, store: reader.avatars, presentation: presentation)
                        }
                        .listRowBackground(presentation.kind == .needsInput ? Color.orange.opacity(0.08) : nil)
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
            .contentMargins(.bottom, rosterBottomMargin, for: .scrollContent)
            .searchable(text: $query, prompt: Text("agents.search"))
            .navigationTitle("agents.title")
            .navigationBarTitleDisplayMode(.inline)
            .refreshable { await reader.refresh() }
            .toolbar {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    Menu {
                        Picker("agents.filter", selection: $filter) {
                            ForEach(AgentListFilter.allCases, id: \.self) { option in
                                Text(LocalizedStringKey("agents.filter." + option.rawValue)).tag(option)
                            }
                        }
                    } label: {
                        Label("agents.filter", systemImage: filter == .all ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
                    }
                    .accessibilityIdentifier("agents.filter")
                    NavigationLink(value: AppRoute.settings) {
                        Label("settings.title", systemImage: "gearshape")
                    }
                    .accessibilityIdentifier("settings.open")
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

private struct AgentSummaryRow: View {
    let agent: ReadingAgent
    let store: AgentAvatarStore
    let presentation: AgentIdentityPresentation
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @ScaledMetric(relativeTo: .headline) private var titleRowHeight: CGFloat = 22
    @ScaledMetric(relativeTo: .subheadline) private var previewRowHeight: CGFloat = 22

    private var hasUnread: Bool { (agent.unreadCount ?? 0) > 0 }
    private var showsInlineStatusBadge: Bool {
        switch presentation.kind {
        case .ready, .unknown: false
        default: true
        }
    }

    var body: some View {
        let preview = AgentSummaryPresentation.preview(agent)
        Group {
            if dynamicTypeSize.isAccessibilitySize {
                HStack(alignment: .top, spacing: 12) {
                    AgentAvatarView(store: store, agentID: agent.id, presentation: presentation)
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text(verbatim: agent.name).font(.headline).foregroundStyle(.primary)
                                .lineLimit(1)
                            Spacer(minLength: 0)
                            if let date = AgentSummaryPresentation.activityDate(agent) {
                                Text(date, style: .relative).font(.caption2).foregroundStyle(.secondary)
                                    .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                            }
                        }
                        HStack(spacing: 4) {
                            if AgentSummaryPresentation.showsOperatorPreview(agent) {
                                Text("reading.inputPreview")
                            }
                            Text(verbatim: preview.isEmpty ? " " : preview)
                        }
                        .font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                        .accessibilityHidden(preview.isEmpty)
                        VStack(alignment: .leading, spacing: 4) {
                            AgentStatusLabel(presentation: presentation, reservedLines: 2)
                            if let unread = agent.unreadCount, unread > 0 {
                                Text("reading.unread \(unread)").fontWeight(.semibold).lineLimit(1)
                            } else {
                                Text(verbatim: " ").lineLimit(1).accessibilityHidden(true)
                            }
                        }
                        .font(.caption).foregroundStyle(.secondary)
                    }
                }
            } else {
                HStack(alignment: .center, spacing: 12) {
                    AgentAvatarView(store: store, agentID: agent.id, presentation: presentation)
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(alignment: .center, spacing: 6) {
                            AgentStatusLabel(presentation: presentation, showsTitle: false)
                                .font(.subheadline.weight(.semibold))
                            Text(verbatim: agent.name)
                                .font(.headline)
                                .foregroundStyle(.primary)
                                .lineLimit(1)
                            if showsInlineStatusBadge {
                                let statusTint = AgentStatusLabel.tint(for: presentation)
                                Text(LocalizedStringKey(presentation.statusKey))
                                    .font(.caption2.weight(.semibold))
                                    .foregroundStyle(statusTint)
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 1.5)
                                    .background(statusTint.opacity(0.14), in: Capsule())
                                    .accessibilityHidden(true)
                            }
                            Spacer(minLength: 4)
                            if let date = AgentSummaryPresentation.activityDate(agent) {
                                Text(date, style: .relative)
                                    .font(.caption)
                                    .foregroundStyle(hasUnread ? Color.accentColor : .secondary)
                                    .lineLimit(1)
                                    .fixedSize(horizontal: true, vertical: false)
                            }
                        }
                        .frame(minHeight: titleRowHeight)
                        HStack(alignment: .center, spacing: 8) {
                            HStack(spacing: 4) {
                                if AgentSummaryPresentation.showsOperatorPreview(agent) {
                                    Text("reading.inputPreview").fontWeight(.medium)
                                }
                                // Keep the same row height when an asynchronously loaded preview arrives.
                                Text(verbatim: preview.isEmpty ? " " : preview)
                            }
                            .font(.subheadline)
                            .foregroundStyle(hasUnread ? Color.primary.opacity(0.85) : Color.secondary)
                            .lineLimit(1)
                            .accessibilityHidden(preview.isEmpty)
                            Spacer(minLength: 0)
                            if let unread = agent.unreadCount, unread > 0 {
                                Text(verbatim: unread > 99 ? "99+" : "\(unread)")
                                    .font(.caption2.weight(.bold))
                                    .monospacedDigit()
                                    .foregroundStyle(.white)
                                    .padding(.horizontal, 6.5)
                                    .padding(.vertical, 2)
                                    .frame(minWidth: 20, minHeight: 20)
                                    .background(Color.accentColor, in: Capsule())
                                    .accessibilityLabel(Text("reading.unread \(unread)"))
                            }
                        }
                        .frame(minHeight: previewRowHeight)
                    }
                }
            }
        }
        .padding(.vertical, 6)
        .accessibilityElement(children: .combine)
    }
}

private struct AgentConversationTitle: View {
    let agent: ReadingAgent
    let store: AgentAvatarStore
    let presentation: AgentIdentityPresentation

    var body: some View {
        HStack(spacing: 6) {
            AgentAvatarView(store: store, agentID: agent.id, presentation: presentation, size: 24)
            VStack(alignment: .leading, spacing: 1) {
                Text(verbatim: agent.name).font(.footnote.weight(.semibold)).lineLimit(1)
                AgentStatusLabel(presentation: presentation)
                    .font(.caption2).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.identity")
    }
}

struct ConversationReadingView: View {
    private enum ScrollRequest: Hashable { case latest }
    @Bindable var reader: ReadingCoordinator
    let sender: SendingCoordinator?
    var apiBaseURL: URL?
    var openReference: (String) -> Void
    var openWork: (String) -> Void
    var openTask: (String) -> Void
    @State private var nearBottom = true
    @State private var follow = ConversationFollowState()
    @State private var newContent = false
    @State private var showConnectionDetails = false
    @State private var initializedAgent: String?
    @State private var readThroughToConfirm: ReadingReadConfirmation?
    @State private var historyEndTurnID: String?
    @State private var visibleTurnIDs: [String] = []
    @State private var userScrolling = false

    private var connectionPresentation: ConversationConnectionPresentation {
        .init(status: reader.status, hasSnapshot: reader.snapshot != nil,
              isCached: reader.isShowingCachedContent, isRefreshing: reader.isConversationRefreshing,
              canPersistDraft: sender?.canPersistDraft == true)
    }
    @State private var scrollRequest: ScrollRequest?
    @State private var scrollPosition = ScrollPosition(idType: String.self)
    @State private var historyScrollIntent: String?
    @State private var showPrivacyInfo = false

    private var turnIDs: [String] { reader.conversationProjection?.turnIDs ?? [] }
    private var turnRange: Range<Int> { ConversationTurnWindow.range(ids: turnIDs, endingAt: historyEndTurnID) }
    private var visibleTurns: ArraySlice<ReadingTurnPresentation> { turns[turnRange] }

    private var turns: [ReadingTurnPresentation] { reader.conversationProjection?.turns ?? [] }

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
        // Post-mount id directives on ScrollPosition are silently dropped in
        // this stack (pre-mount restore assignments resolve fine); route
        // history positioning through ScrollViewReader's imperative proxy.
        ScrollViewReader { scrollProxy in
        ScrollView {
            // The explicit turn window is bounded to20. Use its actual heights:
            // lazy offscreen estimates can loop while scrolling rich results.
            VStack(alignment: .leading, spacing: 16) {
                if turnRange.lowerBound > 0 || reader.canLoadHistory {
                    HStack {
                        Spacer(minLength: 0)
                        Button {
                            follow.reviewHistory()
                            if turnRange.lowerBound > 0 {
                                showHistoryWindow(endingAt: ConversationTurnWindow.olderEnd(ids: turnIDs, current: turnRange))
                            } else {
                                Task {
                                    let agent = reader.selectedAgentID
                                    let epoch = reader.snapshot?.eventLogEpoch
                                    let previousFirst = turnIDs.first
                                    await reader.loadHistory()
                                    guard reader.selectedAgentID == agent, reader.snapshot?.eventLogEpoch == epoch else { return }
                                    if let previousFirst, let index = turnIDs.firstIndex(of: previousFirst), index > 0 {
                                        showHistoryWindow(endingAt: turnIDs[min(turnIDs.count - 1, index + 4)])
                                    } else {
                                        showHistoryWindow(endingAt: historyEndTurnID)
                                    }
                                }
                            }
                        } label: {
                            Label("reading.history", systemImage: "arrow.up.circle")
                                .font(.caption.weight(.medium))
                        }
                        .buttonStyle(.bordered)
                        .buttonBorderShape(.capsule)
                        .controlSize(.small)
                        .disabled(reader.isLoadingHistory || (turnRange.lowerBound == 0 && reader.status != .live))
                        .accessibilityIdentifier("conversation.older")
                        .overlay(alignment: .trailing) {
                            if reader.isLoadingHistory { ProgressView().controlSize(.mini).offset(x: 22) }
                        }
                        Spacer(minLength: 0)
                    }
                }
                if reader.status == .offline && reader.snapshot == nil {
                    Text("reading.cache.noConversation").foregroundStyle(.secondary)
                        .accessibilityIdentifier("conversation.notCached")
                }
                if reader.snapshot != nil && turns.isEmpty && reader.snapshot?.raw["pending_inputs"].viewArray.isEmpty == true {
                    Text("reading.emptyConversation").foregroundStyle(.secondary)
                }
                ForEach(visibleTurns) { turn in
                    ReadingTurnView(turn: turn, reader: reader)
                        .environment(\.holonOpenReference, reader.status == .live ? openReference : nil)
                        .environment(\.holonOpenWork, reader.status == .live ? openWork : nil)
                        .environment(\.holonOpenTask, reader.status == .live ? openTask : nil)
                    // No explicit .id(turn.id) here: ForEach already gives each
                    // turn its Identifiable identity, and an .id() modifier on
                    // children of a scrollTargetLayout breaks ScrollPosition's
                    // id-based scrollTo resolution (the directive is silently
                    // dropped even after the target materializes).
                }
                if turnRange.upperBound < turns.count {
                    HStack {
                        Spacer(minLength: 0)
                        Button {
                            let ids = turnIDs
                            let nextEnd = ConversationTurnWindow.newerEnd(ids: ids, current: turnRange)
                            showHistoryWindow(endingAt: nextEnd)
                        } label: {
                            Label("reading.newerTurns", systemImage: "arrow.down.circle")
                                .font(.caption.weight(.medium))
                        }
                        .buttonStyle(.bordered)
                        .buttonBorderShape(.capsule)
                        .controlSize(.small)
                        .accessibilityIdentifier("conversation.newer")
                        Spacer(minLength: 0)
                    }
                }
                ForEach((reader.snapshot?.raw["pending_inputs"].viewArray ?? []).filter { $0["presentation_class"] == .string("operator") && TaskResultInput($0) == nil }, id: \.viewMessageID) { input in
                    ReadingInputView(input: input, reader: reader)
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
        .accessibilityIdentifier("conversation.viewport")
        .scrollPosition($scrollPosition)
        // Initial layout must not override a saved historical ID with the tail.
        .defaultScrollAnchor(reader.readingPosition == nil ? .bottom : .top, for: .initialOffset)
        // Only tail following needs a resize anchor. Historical windows retain
        // their native offset/explicit ID instead of competing with a top anchor.
        .defaultScrollAnchor(follow.followsLatest && historyEndTurnID == nil ? .bottom : nil, for: .sizeChanges)
        .onScrollTargetVisibilityChange(idType: String.self, threshold: 0.1) { ids in
            visibleTurnIDs = ids
            // Arrival confirmation: once the layout reports the pending target
            // visible, retire the intent and re-affirm the anchor (the early
            // consumer below may have travelled a long distance to get here).
            guard let target = historyScrollIntent, ids.contains(target) else { return }
            historyScrollIntent = nil
            scrollProxy.scrollTo(target, anchor: .top)
        }
        .onChange(of: historyScrollIntent) { _, target in
            guard let target else { return }
            // The turn window is a plain VStack: every turn id registers when
            // the navigation transaction commits, so the imperative proxy can
            // resolve the target even while it is off-screen. Jumping to a
            // window top from the window bottom never brings the target
            // through the visibility set on its own, so consume early and let
            // the visibility callback above confirm arrival.
            Task { @MainActor in
                guard historyScrollIntent == target else { return }
                scrollProxy.scrollTo(target, anchor: .top)
            }
        }
        .onScrollPhaseChange { _, phase in
            if phase == .tracking || phase == .interacting {
                userScrolling = true
                follow.userBeganScroll()
                scrollRequest = nil
            }
            if phase == .interacting {
                // Only a real drag revokes pending programmatic positioning;
                // the navigation tap itself never reports a tracking phase.
                historyScrollIntent = nil
            }
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
            guard scrollRequest != nil else { return }
            guard follow.followsLatest, !userScrolling else { scrollRequest = nil; return }
            scrollPosition.scrollTo(edge: .bottom)
            scrollRequest = nil
        }
        .onChange(of: latestContentKey) { _, _ in
            if follow.followsLatest, nearBottom, historyEndTurnID == nil { scrollRequest = .latest }
            else { newContent = true }
        }
        .onChange(of: reader.snapshot?.eventLogEpoch) { old, new in
            if let old, let new, old != new { follow.showLatest(); historyEndTurnID = nil; scrollRequest = .latest }
        }
        .overlay(alignment: .bottom) {
            if !nearBottom || historyEndTurnID != nil || !follow.followsLatest {
                Button {
                    follow.showLatest()
                    historyEndTurnID = nil; scrollRequest = .latest; newContent = false
                    if let latest = turnIDs.last { reader.rememberPosition(turnID: latest) }
                } label: {
                    Label(newContent ? "reading.newContent" : "reading.latest", systemImage: "arrow.down")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(newContent ? Color.accentColor : .primary)
                        .padding(.horizontal, 14).padding(.vertical, 9)
                        .background(.regularMaterial, in: Capsule())
                        .overlay(Capsule().strokeBorder(Color.primary.opacity(0.1), lineWidth: 0.5))
                        .shadow(color: .black.opacity(0.08), radius: 6, y: 2)
                }
                .buttonStyle(.plain)
                .frame(minHeight: 44)
                .accessibilityIdentifier("conversation.latest")
                .padding(.bottom, 8)
            }
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
            historyScrollIntent = nil
            visibleTurnIDs = []
            userScrolling = false
            newContent = false
            initializedAgent = agent
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) {
            VStack(spacing: 0) {
                if let sender {
                    SendingView(sender: sender, reader: reader, currentRunID: currentRunID)
                } else {
                    Text("status.storageError").font(.caption).padding()
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .principal) {
                HStack(spacing: 0) {
                    if let agent = reader.agents.first(where: { $0.id == reader.selectedAgentID }) {
                        AgentConversationTitle(agent: agent, store: reader.avatars, presentation: .derive(
                            agent, status: reader.status, isRosterCurrent: reader.hasDisplayableLiveRoster,
                            isCached: reader.rosterIsCached,
                            isRefreshing: reader.isInitialRosterRefreshing
                        ))
                    } else {
                        VStack(alignment: .leading, spacing: 1) {
                            Text(verbatim: reader.selectedAgentID ?? "Holon").lineLimit(1)
                            AgentStatusLabel(presentation: .init(kind: .unknown)).font(.caption2)
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityIdentifier("conversation.identity")
                    }
                    ConversationConnectionButton(presentation: connectionPresentation) {
                        showConnectionDetails = true
                    }
                    .layoutPriority(1)
                }
            }
            ToolbarItemGroup(placement: .primaryAction) {
                    if reader.status == .live, let agent = reader.selectedAgentID {
                        NavigationLink(value: AppRoute.work(agent)) { Label("work.title", systemImage: "checklist") }
                            .labelStyle(.iconOnly)
                            .accessibilityIdentifier("conversation.work")
                        NavigationLink(value: AppRoute.files(agent)) { Label("files.title", systemImage: "folder") }
                            .labelStyle(.iconOnly)
                            .accessibilityIdentifier("conversation.files")
                    }
                Menu {
                    Button { Task { await reader.refresh() } } label: {
                        Label("reading.refresh", systemImage: "arrow.clockwise")
                    }
                    Button { readThroughToConfirm = reader.readConfirmationForVisibleBriefs } label: {
                        Label("reading.markRead", systemImage: "checkmark.circle")
                    }
                    .disabled(reader.readThroughForVisibleBriefs == nil)
                    Divider()
                    NavigationLink(value: AppRoute.tools) { Label("settings.sharing", systemImage: "square.and.arrow.up") }
                    NavigationLink(value: AppRoute.diagnostics) { Label("diagnostics.title", systemImage: "stethoscope") }
                        .accessibilityIdentifier("diagnostics.open")
                    Button { showPrivacyInfo = true } label: { Label("privacy.aiInfo", systemImage: "hand.raised") }
                        .accessibilityIdentifier("conversation.privacyInfo")
                } label: { Image(systemName: "ellipsis.circle").accessibilityLabel(Text("conversation.more")) }
                .accessibilityIdentifier("conversation.more")
            }
        }
        .sheet(isPresented: $showConnectionDetails) {
            NavigationStack {
                ScrollView {
                    ConversationConnectionDetails(presentation: connectionPresentation) {
                        Task { await reader.refresh() }
                    }
                    .padding(20)
                }
                .navigationTitle("reading.connectionDetails")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("files.dismiss") { showConnectionDetails = false }
                            .accessibilityIdentifier("conversation.connectionClose")
                    }
                }
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

    private func showHistoryWindow(endingAt end: String?) {
        let ids = turnIDs
        let range = ConversationTurnWindow.range(ids: ids, endingAt: end)
        guard !range.isEmpty else { return }
        let first = ids[range.lowerBound]
        follow.reviewHistory()
        scrollRequest = nil
        historyEndTurnID = end
        // Post-mount ScrollPosition id directives are dropped in this stack,
        // so both the same-window and replacement-window paths defer
        // positioning to the imperative proxy consumers above.
        historyScrollIntent = first
        reader.rememberPosition(turnID: first)
    }

    private var latestContentKey: String {
        let latest = turns.last
        let loadedBriefs = latest?.raw["brief_ids"].viewArray.compactMap(\.viewString).map { id in
            "\(id):\(reader.briefs[id].map { BriefPresentation.text($0).utf8.count } ?? -1)"
        }.joined(separator: "|") ?? ""
        return "\(latest?.id ?? "")|\(latest?.raw["revision"].viewJSON ?? "")|\(loadedBriefs)|\(reader.snapshot?.raw["pending_inputs"].viewJSON ?? "")|\(sender?.entries.count ?? 0)"
    }
}

private struct ReadingTurnView: View {
    let turn: ReadingTurnPresentation
    @Bindable var reader: ReadingCoordinator
    @State private var showActivities = false
    @State private var fullActivities = false
    @Environment(\.holonOpenTask) private var openTask
    @State private var reportScope: ContentReportScope?

    private var operatorInputs: [JSONValue] {
        turn.raw["inputs"].viewArray.filter {
            ($0["presentation_class"].viewString ?? turn.raw["presentation_class"].viewString) == "operator"
        }
    }

    private var visibleBriefIDs: [String] {
        turn.raw["brief_ids"].viewArray.compactMap(\.viewString)
    }

    private var isCompactBackgroundTurn: Bool {
        !showActivities
            && ReadingPresentation.turnStatus(turn.raw) == nil
            && operatorInputs.isEmpty
            && visibleBriefIDs.isEmpty
            && TaskResultInput.header(turn.raw["inputs"].viewArray)?.isFailure != true
    }

    var body: some View {
        let source = ReadingPresentation.turnSource(turn.raw)
        VStack(alignment: .leading, spacing: isCompactBackgroundTurn ? 6 : 12) {
            HStack(spacing: 6) {
                Image(systemName: source.symbol)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(Text(LocalizedStringKey(source.key)))
                Text(verbatim: "#\(turn.index)")
                    .monospacedDigit()
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
                if let date = turn.raw["started_at"].viewString.flatMap(ReadingPresentation.date) {
                    Text(date, format: .dateTime.month(.abbreviated).day().hour().minute())
                        .accessibilityIdentifier("conversation.time." + turn.id)
                }
                Spacer(minLength: 4)
                if let status = ReadingPresentation.turnStatus(turn.raw) {
                    Text(LocalizedStringKey(status))
                        .fontWeight(.medium)
                        .foregroundStyle(ReadingPresentation.turnStatusColor(status))
                }
                if let duration = ReadingPresentation.turnDuration(turn.raw) {
                    Text(verbatim: duration)
                        .monospacedDigit()
                        .foregroundStyle(.tertiary)
                }
            }
            .font(.caption2).foregroundStyle(.secondary)
            ForEach(turn.raw["inputs"].viewArray, id: \.viewMessageID) { input in
                ReadingInputView(input: input, reader: reader, fallbackClass: turn.raw["presentation_class"].viewString)
            }
            ForEach(visibleBriefIDs, id: \.self) { briefID in
                ReadingBriefView(briefID: briefID, reader: reader, inputs: turn.raw["inputs"].viewArray,
                                 onReport: { reportScope = reader.contentReportScope })
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
                .frame(maxWidth: .infinity, minHeight: isCompactBackgroundTurn ? 36 : 44, alignment: .leading)
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
    let reader: ReadingCoordinator
    var fallbackClass: String? = nil
    @State private var showsFullMessage = false

    var body: some View {
        if (input["presentation_class"].viewString ?? fallbackClass) == "operator" {
            let preview = OperatorInputPresentation.decodePreview(input["preview"].viewString ?? "")
            OperatorMessageBubble {
                VStack(alignment: .leading, spacing: 6) {
                    OperatorMessageText(text: preview.text)
                    if !preview.isTruncated, !input.viewMessageID.isEmpty {
                        ReadingOperatorInlineImages(reader: reader, messageID: input.viewMessageID, text: preview.text)
                    }
                    if preview.isTruncated, !input.viewMessageID.isEmpty {
                        Button("reading.fullMessage") { showsFullMessage = true }
                            .font(.callout)
                            .disabled(reader.status != .live)
                            .accessibilityIdentifier("operator.fullMessage." + input.viewMessageID)
                    }
                    if let actor = input["actor_display_name"].viewString?.trimmingCharacters(in: .whitespacesAndNewlines), !actor.isEmpty {
                        Text(verbatim: actor).font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
            .accessibilityIdentifier("operator." + input.viewMessageID)
            .sheet(isPresented: $showsFullMessage) {
                if let agentID = reader.selectedAgentID {
                    ReadingFullOperatorMessage(reader: reader, agentID: agentID, messageID: input.viewMessageID)
                }
            }
        }
    }
}

private struct ReadingOperatorInlineImages: View {
    private struct Request: Equatable {
        let scope: Int
        let agentID: String
        let messageID: String
        let attempt: Int
    }

    let reader: ReadingCoordinator
    let messageID: String
    let text: String
    @Environment(\.holonConversationImageAccess) private var imageAccess
    private enum Resolution {
        case message(OperatorInputPresentation.Message), deferred, failed
    }
    @State private var resolved: (Request, Resolution)?
    @State private var attempt = 0

    var body: some View {
        if !ConversationImageReference.operatorTailReferences(text).isEmpty,
           let scope = imageAccess?.scope, let agentID = reader.selectedAgentID {
            let request = Request(scope: scope, agentID: agentID, messageID: messageID, attempt: attempt)
            Group {
                if let resolved, resolved.0 == request {
                    switch resolved.1 {
                    case .message(let message):
                        ReadingOperatorImages(message: message)
                            .id(messageID)
                    case .deferred:
                        Button("reading.imageLoadPreview") { attempt &+= 1 }
                            .font(.callout)
                    case .failed:
                        Button("reading.retry") { attempt &+= 1 }
                            .font(.callout)
                    }
                } else {
                    ProgressView("reading.imageLoading").font(.caption)
                }
            }
            .task(id: request) {
                do {
                    let message = try await reader.loadInlineImageMessage(messageID, agentID: agentID)
                    try Task.checkCancellation()
                    resolved = (request, .message(message))
                } catch {
                    guard !Task.isCancelled else { return }
                    let deferred = error is ConversationPreviewDeferred || error is CancellationError
                    resolved = (request, deferred ? .deferred : .failed)
                }
            }
        }
    }
}

private struct ReadingOperatorImages: View {
    let message: OperatorInputPresentation.Message
    @State private var loadLegacyReferences = false

    var body: some View {
        if let uploadedImages = message.uploadedImages {
            ConversationImagePreviews(images: uploadedImages)
        } else {
            let references = ConversationImageReference.operatorTailReferences(message.text)
            if !references.isEmpty {
                if loadLegacyReferences {
                    ConversationImagePreviews(images: references)
                } else {
                    Button("reading.loadImageReferences") { loadLegacyReferences = true }
                        .font(.callout)
                        .accessibilityIdentifier("conversation.image.loadReferences")
                }
            }
        }
    }
}

private struct ReadingFullOperatorMessage: View {
    let reader: ReadingCoordinator
    let agentID: String
    let messageID: String
    @Environment(\.dismiss) private var dismiss
    @Environment(\.holonOpenReference) private var openReference
    @State private var message: OperatorInputPresentation.Message?
    @State private var didFail = false
    @State private var attempt = 0

    var body: some View {
        NavigationStack {
            Group {
                if let message {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 12) {
                            ReadingOperatorImages(message: message)
                                .environment(\.holonOpenReference, openReference.map { open in
                                    { reference in
                                        dismiss()
                                        open(reference)
                                    }
                                })
                            OperatorMessageText(text: message.text)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding()
                    }
                } else if didFail {
                    VStack(spacing: 12) {
                        Text("reading.fullMessageUnavailable")
                        Button("reading.retry") {
                            didFail = false
                            attempt += 1
                        }.disabled(reader.status != .live)
                    }.padding()
                } else {
                    ProgressView()
                }
            }
            .navigationTitle("reading.fullMessage")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("files.dismiss") { dismiss() }
                }
            }
        }
        .task(id: attempt) {
            do {
                let loaded = try await reader.loadOperatorMessage(messageID, agentID: agentID)
                try Task.checkCancellation()
                message = loaded
            } catch {
                guard !Task.isCancelled else { return }
                didFail = true
            }
        }
        .onChange(of: reader.selectedAgentID) { _, selected in
            if selected != agentID { dismiss() }
        }
        .onChange(of: reader.status) { _, status in
            if status == .sessionExpired || status == .permissionDenied { dismiss() }
        }
    }
}

/// The connection is independent of the Agent's scheduling state.
struct ConversationConnectionPresentation {
    let status: ReadingStatus
    let hasSnapshot: Bool
    var isCached = false
    var isRefreshing = false
    var canPersistDraft = false
    var key: String {
        ReadingPresentation.connectionStatusKey(
            status: status, hasContent: hasSnapshot, isCached: isCached, isRefreshing: isRefreshing
        ) ?? "reading.status.live"
    }
    var showsDraftNotice: Bool { status != .live && canPersistDraft }
    var showsRetry: Bool {
        ReadingPresentation.showsConnectionRetry(status: status, isRefreshing: isRefreshing)
    }
    var isError: Bool {
        switch status {
        case .permissionDenied, .sessionExpired, .incompatible: true
        default: false
        }
    }
    var isLoading: Bool {
        !isError && (isRefreshing || status == .syncing || (status == .live && !hasSnapshot))
    }
    var symbol: String {
        if isError { return "exclamationmark.circle" }
        if status != .live { return "wifi.exclamationmark" }
        return isCached ? "internaldrive" : "dot.radiowaves.left.and.right"
    }
}

struct ConversationConnectionButton: View {
    let presentation: ConversationConnectionPresentation
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            Group {
                if presentation.isLoading {
                    ProgressView().controlSize(.mini)
                } else {
                    Image(systemName: presentation.symbol)
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(presentation.isError || presentation.showsRetry ? Color.orange : Color.secondary.opacity(0.65))
                }
            }
            .frame(width: 44, height: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("reading.connectionDetails"))
        .accessibilityValue(Text(LocalizedStringKey(presentation.key)))
        .accessibilityIdentifier("conversation.connectionStatus")
    }
}

struct ConversationConnectionDetails: View {
    let presentation: ConversationConnectionPresentation
    let retry: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(LocalizedStringKey(presentation.key)).font(.headline)
                .accessibilityIdentifier("conversation.connectionMessage")
            if presentation.showsDraftNotice {
                Text("reading.cache.readOnly").foregroundStyle(.secondary)
                    .accessibilityIdentifier("conversation.readOnly")
            }
            if presentation.showsRetry {
                Button("reading.retry", action: retry)
                    .buttonStyle(.borderedProminent)
                    .frame(minHeight: 44)
                    .accessibilityIdentifier("conversation.retry")
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct OperatorMessageBubble<Content: View>: View {
    @ViewBuilder var content: () -> Content
    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            Spacer(minLength: 28)
            content()
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(
                    Color.accentColor.opacity(0.11),
                    in: UnevenRoundedRectangle(topLeadingRadius: 18, bottomLeadingRadius: 18,
                                               bottomTrailingRadius: 4, topTrailingRadius: 18)
                )
                .overlay(
                    UnevenRoundedRectangle(topLeadingRadius: 18, bottomLeadingRadius: 18,
                                           bottomTrailingRadius: 4, topTrailingRadius: 18)
                        .strokeBorder(Color.accentColor.opacity(0.2), lineWidth: 0.5)
                )
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
                        Button("work.details", systemImage: "checklist") { openWork?(workID) }
                            .font(.caption.weight(.medium))
                            .buttonStyle(.bordered)
                            .buttonBorderShape(.capsule)
                            .controlSize(.small)
                            .frame(minHeight: 44, alignment: .leading)
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
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("brief." + briefID)
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

    @MainActor private static let fractionalDateFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
    @MainActor private static let wholeSecondDateFormatter = ISO8601DateFormatter()

    @MainActor static func date(_ value: String) -> Date? {
        fractionalDateFormatter.date(from: value) ?? wholeSecondDateFormatter.date(from: value)
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

    static func turnStatusColor(_ statusKey: String) -> Color {
        switch statusKey {
        case "reading.activeTurn": .accentColor
        case "reading.wait": .orange
        case "reading.failedTurn", "reading.interruptedTurn": .red
        default: .secondary
        }
    }

    static func turnSource(_ turn: JSONValue) -> (symbol: String, key: String) {
        switch turn["presentation_class"]?.readingString {
        case "operator": ("person", "reading.source.operator")
        case "task": ("wrench.and.screwdriver", "reading.source.task")
        case "external": ("bubble.left", "reading.source.external")
        case "timer": ("clock", "reading.source.timer")
        case "internal": ("arrow.triangle.branch", "reading.source.internal")
        case "system": ("cpu", "reading.source.system")
        case "operational": ("wrench.and.screwdriver", "reading.source.operational")
        default: ("circle.dotted", "reading.turn")
        }
    }

    @MainActor static func turnDuration(_ turn: JSONValue) -> String? {
        guard turn["execution"]?["kind"] != .string("active"),
              let startText = turn["started_at"]?.readingString,
              let endText = turn["completed_at"]?.readingString
                ?? turn["ended_at"]?.readingString
                ?? turn["execution"]?["completed_at"]?.readingString
                ?? turn["execution"]?["ended_at"]?.readingString,
              let start = date(startText), let end = date(endText) else { return nil }
        let seconds = Int(end.timeIntervalSince(start).rounded())
        guard seconds >= 1 else { return nil }
        if seconds < 60 { return "\(seconds)s" }
        let minutes = seconds / 60, rem = seconds % 60
        if minutes < 60 { return rem == 0 ? "\(minutes)m" : "\(minutes)m\(rem)s" }
        let hours = minutes / 60, remMin = minutes % 60
        return remMin == 0 ? "\(hours)h" : "\(hours)h\(remMin)m"
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
