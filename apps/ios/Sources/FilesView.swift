import SwiftUI

/// The browser keeps its filter, root and scroll position when a reader is pushed.
struct FilesView: View {
    @Bindable var coordinator: FilesCoordinator
    var openFile: (FilesRequest) -> Void
    @State private var enteringReference = false
    @State private var reference = ""

    var body: some View {
        List {
            if let directory = coordinator.directory {
                Section {
                    breadcrumbs(directory)
                    ForEach(coordinator.entries) { entry in
                        if entry.isDirectory {
                            Button { coordinator.browse(directory.workspace, path: entry.path) } label: { row(entry) }
                                .id(entry.id)
                        } else {
                            NavigationLink(value: AppRoute.file(coordinator.selectedAgentID ?? "",
                                .source(.workspace(directory.workspace, path: entry.path)))) { row(entry) }
                                .id(entry.id).accessibilityIdentifier("files.entry." + entry.path)
                        }
                    }
                    if coordinator.entries.isEmpty { Text("files.empty").foregroundStyle(.secondary) }
                } header: { Text(directory.workspace.name) }
            } else {
                Section("files.workspaces") {
                    ForEach(coordinator.workspaces) { workspace in
                        Button { coordinator.browse(workspace) } label: {
                            Label(workspace.name, systemImage: "externaldrive")
                        }
                    }
                }
            }
            if coordinator.isLoading { ProgressView("work.loading") }
            if let failure = coordinator.failure {
                Text(LocalizedStringKey(failure.key)).foregroundStyle(.secondary)
                Button("work.retry") { coordinator.selectAgent(coordinator.selectedAgentID) }
            }
        }
        .scrollPosition(id: $coordinator.directoryPosition)
        .navigationTitle("files.title")
        .searchable(text: $coordinator.query, prompt: "files.filter")
        .toolbar {
            Menu {
                Toggle("files.showHidden", isOn: $coordinator.showHidden)
                Picker("files.sort", selection: $coordinator.sort) {
                    Text("files.sort.name").tag(FilesSort.name)
                    Text("files.sort.modified").tag(FilesSort.modified)
                }
                Button("files.reference") { enteringReference = true }
                Button("files.workspaces") { coordinator.selectAgent(coordinator.selectedAgentID) }
            } label: { Label("files.options", systemImage: "ellipsis") }
                .accessibilityIdentifier("files.options")
        }
        .sheet(isPresented: $enteringReference) {
            NavigationStack {
                Form {
                    TextField("files.reference.placeholder", text: $reference)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .accessibilityIdentifier("files.reference")
                    Button("files.openReference") {
                        enteringReference = false; openFile(.source(.reference(reference)))
                    }.disabled(reference.isEmpty).accessibilityIdentifier("files.openReference")
                }
                .navigationTitle("files.reference")
                .toolbar { Button("action.cancel") { enteringReference = false } }
            }
        }
    }

    private func row(_ entry: FilesEntry) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: entry.isDirectory ? "folder" : "doc").foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: entry.name).foregroundStyle(.primary).lineLimit(2)
                HStack {
                    if !entry.isDirectory, let size = entry.size {
                        Text(ByteCountFormatter.string(fromByteCount: size, countStyle: .file))
                    }
                    if let date = entry.modified { Text(date, format: .dateTime.month().day().hour().minute()) }
                }.font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private func breadcrumbs(_ directory: FilesDirectory) -> some View {
        ScrollView(.horizontal) {
            HStack {
                Button(directory.workspace.name) { coordinator.browse(directory.workspace) }
                let parts = directory.path.split(separator: "/").map(String.init)
                ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                    Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.secondary)
                    Button(part) { coordinator.browse(directory.workspace, path: parts.prefix(index + 1).joined(separator: "/")) }
                }
            }.font(.caption)
        }.scrollIndicators(.hidden)
    }
}
