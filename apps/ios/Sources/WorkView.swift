import SwiftUI
import HolonClient

struct WorkView: View {
    @Bindable var coordinator: WorkCoordinator
    var openPlan: (String, String, JSONValue) -> Void
    var openArtifact: (String, JSONValue) -> Void
    @State private var path: [WorkRoute] = []
    @State private var agentInput = ""

    var body: some View {
        NavigationStack(path: $path) {
            List {
                Section {
                    TextField("work.agent", text: $agentInput)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("work.select_agent") {
                        coordinator.selectAgent(agentInput.isEmpty ? nil : agentInput)
                    }
                    if let agent = coordinator.selectedAgentID { Text(agent).font(.caption) }
                }
                Section("work.items") {
                    state(coordinator.itemsState, empty: coordinator.items.isEmpty)
                    ForEach(coordinator.items) { item in
                        NavigationLink(value: WorkRoute.item(item.id)) { row(item) }
                            .accessibilityIdentifier("work.item." + item.id)
                    }
                }
                Section("work.tasks") {
                    state(coordinator.tasksState, empty: coordinator.tasks.isEmpty)
                    ForEach(coordinator.tasks) { task in
                        NavigationLink(value: WorkRoute.task(task.id)) { row(task) }
                            .accessibilityIdentifier("work.task." + task.id)
                    }
                }
            }
            .navigationTitle("work.title")
            .toolbar {
                Button("work.refresh", systemImage: "arrow.clockwise") { coordinator.refresh() }
            }
            .navigationDestination(for: WorkRoute.self) { route in
                detail(route)
                    .onAppear { coordinator.open(route) }
            }
        }
        .onAppear { agentInput = coordinator.selectedAgentID ?? "" }
        .onChange(of: coordinator.selectedAgentID) { _, id in
            path = []
            agentInput = id ?? ""
        }
    }

    private func row(_ record: WorkRecord) -> some View {
        VStack(alignment: .leading) {
            Text(record.title).lineLimit(3)
            Text(record.state).font(.caption).foregroundStyle(.secondary)
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
                    Section("work.details") {
                        Text(record.title).textSelection(.enabled)
                        LabeledContent("work.status", value: record.state)
                        ForEach(["readiness", "scheduling_state", "focus", "blocked_by", "result_summary"],
                                id: \.self) { key in
                            if let text = record.raw[key]?.workString {
                                VStack(alignment: .leading) {
                                    Text(LocalizedStringKey("work.\(key)")).font(.caption)
                                    Text(text).textSelection(.enabled)
                                }
                            }
                        }
                    }
                    if case .item = route {
                        plan(record)
                        if let id = record.briefID {
                            NavigationLink("work.brief", value: WorkRoute.brief(id))
                        }
                        if !record.references.isEmpty {
                            Section("work.artifacts") {
                                ForEach(Array(record.references.prefix(20).enumerated()), id: \.offset) { _, ref in
                                    Button {
                                        if let agent = coordinator.selectedAgentID { openArtifact(agent, ref) }
                                    } label: {
                                        Text(ref["title"]?.workString ?? ref["ref"]?.workString ?? "")
                                    }
                                }
                            }
                        }
                    }
                }
                if let output = coordinator.output {
                    Section("work.output") {
                        LabeledContent("work.status", value: output.status)
                        if output.truncated { Text("work.truncated").foregroundStyle(.secondary) }
                        if let text = output.text {
                            Text(text).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                                .accessibilityIdentifier("work.output")
                        } else { Text("work.no_output") }
                    }
                }
                if let brief = coordinator.brief {
                    Section("work.brief") {
                        Text(verbatim: BriefPresentation.text(brief))
                            .textSelection(.enabled)
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
                if let preview = plan["preview"]?.workString {
                    Text(preview).textSelection(.enabled)
                    if plan["preview_complete"] != .bool(true) { Text("work.truncated") }
                } else { Text("work.no_preview") }
                if plan["workspace_id"]?.workString != nil,
                   plan["relative_path"]?.workString != nil {
                    Button("work.open_plan") {
                        if let agent = coordinator.selectedAgentID { openPlan(agent, record.id, plan) }
                    }
                    .accessibilityIdentifier("work.openPlan")
                } else { Text("work.plan_unavailable") }
            } else if status != "missing" && status != "unreadable" {
                Text("work.no_plan")
            }
        }
    }
}
