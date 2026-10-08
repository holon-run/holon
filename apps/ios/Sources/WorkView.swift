import SwiftUI
import HolonClient

struct WorkView: View {
    @Bindable var coordinator: WorkCoordinator
    var route: WorkRoute? = nil
    var openReference: ((String) -> Void)? = nil
    var openPlan: (String, String, JSONValue) -> Void
    var openArtifact: (String, JSONValue) -> Void
    @State private var filter = "all"

    var body: some View {
        Group {
            if let route {
                detail(route).onAppear { coordinator.open(route); coordinator.setDetailVisible(true) }
                    .onDisappear { coordinator.setDetailVisible(false, route: route) }
            } else {
            List {
                Picker("work.filter", selection: $filter) {
                    Text("work.filter.all").tag("all")
                    Text("work.filter.active").tag("active")
                    Text("work.filter.completed").tag("completed")
                }.pickerStyle(.segmented)
                Section("work.items") {
                    state(coordinator.itemsState, empty: coordinator.items.isEmpty)
                    ForEach(coordinator.items.filter { filter == "all" || ($0.completed == (filter == "completed")) }) { item in
                        NavigationLink(value: AppRoute.workDetail(coordinator.selectedAgentID ?? "", .item(item.id))) { row(item) }
                            .accessibilityIdentifier("work.item." + item.id)
                    }
                    if coordinator.items.count == coordinator.itemLimit {
                        if coordinator.itemLimit < 400 { Button("work.loadMore") { coordinator.loadMoreItems() } }
                        else { Text("work.windowLimit").font(.caption).foregroundStyle(.secondary) }
                    }
                }
                Section("work.tasks") {
                    state(coordinator.tasksState, empty: coordinator.tasks.isEmpty)
                    ForEach(coordinator.tasks) { task in
                        NavigationLink(value: AppRoute.workDetail(coordinator.selectedAgentID ?? "", .task(task.id))) { row(task) }
                            .accessibilityIdentifier("work.task." + task.id)
                    }
                    if coordinator.tasks.count == coordinator.taskLimit {
                        if coordinator.taskLimit < 400 { Button("work.loadMore") { coordinator.loadMoreTasks() } }
                        else { Text("work.windowLimit").font(.caption).foregroundStyle(.secondary) }
                    }
                }
            }
            .navigationTitle("work.title")
            .toolbar {
                Button("work.refresh", systemImage: "arrow.clockwise") { coordinator.refresh() }
            }
            }
        }
    }

    private func row(_ record: WorkRecord) -> some View {
        VStack(alignment: .leading) {
            Text(record.title).lineLimit(3)
            if let step = record.nextStep { Text(step).font(.subheadline).foregroundStyle(.secondary).lineLimit(2) }
            Text(LocalizedStringKey(record.stateKey)).font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func state(_ value: WorkLoadState, empty: Bool = false) -> some View {
        switch value {
        case .loading: ProgressView("work.loading")
        case .failed: Text("work.error").foregroundStyle(.secondary)
        case .disconnected: Text("work.disconnected")
        case .offline: Text("work.offline")
        case .incompatible: Text("work.incompatible")
        case .idle: Text("work.select_agent")
        case .loaded:
            if empty { Text("work.empty").foregroundStyle(.secondary) }
        }
    }

    private func detail(_ route: WorkRoute) -> some View {
        List {
            state(coordinator.detailState)
            if coordinator.route == route {
                if let record = coordinator.detail {
                    Section("work.objective") {
                        Text(verbatim: record.title).font(.headline).textSelection(.enabled)
                        Text(LocalizedStringKey(record.stateKey)).font(.caption).foregroundStyle(.secondary)
                        if let blocked = record.raw["blocked_by"]?.workString, !blocked.isEmpty {
                            Label(blocked, systemImage: "pause.circle").foregroundStyle(.secondary)
                        }
                    }
                    if !record.todos.isEmpty {
                        Section("work.steps") {
                            ForEach(Array(record.todos.enumerated()), id: \.offset) { _, todo in
                                HStack(alignment: .top) {
                                    Image(systemName: todo["state"] == .string("completed") ? "checkmark.circle" : "circle")
                                        .foregroundStyle(.secondary)
                                    Text(verbatim: todo["text"]?.workString ?? "").textSelection(.enabled)
                                }
                            }
                        }
                    }
                    if coordinator.brief == nil, let result = record.raw["result_summary"]?.workString {
                        Section("work.brief") { RichTextContent(text: result, openReference: openReference) }
                    }
                    if let brief = coordinator.brief {
                        Section("work.brief") {
                            RichTextContent(text: BriefPresentation.text(brief), openReference: openReference)
                        }
                    }
                    if case .item = route {
                        if coordinator.briefFailed {
                            Text("work.briefError").foregroundStyle(.secondary)
                            Button("work.retry") { coordinator.open(route) }
                        }
                        if !record.references.isEmpty {
                            Section("work.artifacts") {
                                ForEach(Array(record.references.enumerated()), id: \.offset) { _, ref in
                                    Button {
                                        if let agent = coordinator.selectedAgentID { openArtifact(agent, ref) }
                                    } label: {
                                        Text(ref["title"]?.workString ?? ref["ref"]?.workString ?? "")
                                    }
                                }
                            }
                        }
                        plan(record)
                    }
                    Section {
                        DisclosureGroup("work.metadata") {
                            ForEach(["readiness", "scheduling_state", "focus", "plan_status", "created_at", "updated_at"], id: \.self) { key in
                                if let text = record.raw[key]?.workString {
                                    LabeledContent(LocalizedStringKey("work.\(key)"), value: text)
                                }
                            }
                            LabeledContent("work.id", value: record.id).textSelection(.enabled)
                        }
                    }
                }
                if let output = coordinator.output {
                    Section("work.output") {
                        if output.truncated { Text("work.truncated").foregroundStyle(.secondary) }
                        if coordinator.outputFailed {
                            Text("work.outputRefreshError").font(.caption).foregroundStyle(.secondary)
                            Button("work.retry") { coordinator.open(route) }
                        }
                        if let text = output.text {
                            Text(text).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                                .accessibilityIdentifier("work.output")
                        } else { Text("work.no_output") }
                    }
                }
                if coordinator.detail == nil, let brief = coordinator.brief {
                    Section("work.brief") {
                        RichTextContent(text: BriefPresentation.text(brief), openReference: openReference)
                    }
                }
            }
            if coordinator.detailState == .failed {
                Button("work.retry") { coordinator.open(route) }
            }
        }
        .navigationTitle("work.details")
    }

    @ViewBuilder
    private func plan(_ record: WorkRecord) -> some View {
        Section("work.plan") {
            let status = record.raw["plan_artifact_status"]?.workString ?? "not_recorded"
            if status == "missing" { Text("work.plan_missing") }
            if status == "unreadable" { Text("work.plan_unreadable") }
            if let plan = record.plan {
                if plan["workspace_id"]?.workString != nil,
                   plan["relative_path"]?.workString != nil {
                    Button("work.open_plan") {
                        if let agent = coordinator.selectedAgentID { openPlan(agent, record.id, plan) }
                    }
                    .accessibilityIdentifier("work.openPlan")
                } else { Text("work.plan_unavailable") }
                if let preview = plan["preview"]?.workString {
                    Text(verbatim: String(preview.prefix(16_384))).lineLimit(6).foregroundStyle(.secondary)
                } else { Text("work.no_preview") }
            } else if status != "missing" && status != "unreadable" {
                Text("work.no_plan")
            }
        }
    }
}
