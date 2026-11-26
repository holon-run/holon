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
                            Button { coordinator.browse(directory.workspace, path: entry.path) } label: {
                                HStack(spacing: 8) {
                                    row(entry)
                                    Spacer(minLength: 0)
                                    Image(systemName: "chevron.right")
                                        .font(.caption.weight(.semibold))
                                        .foregroundStyle(.tertiary)
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .id(entry.id).accessibilityIdentifier("files.entry." + entry.path)
                        } else {
                            NavigationLink(value: AppRoute.file(coordinator.selectedAgentID ?? "",
                                .source(.workspace(directory.workspace, path: entry.path)))) { row(entry) }
                                .id(entry.id).accessibilityIdentifier("files.entry." + entry.path)
                        }
                    }
                    if coordinator.entries.isEmpty { Text("files.empty").foregroundStyle(.secondary) }
                }
            } else {
                Section("files.workspaces") {
                    ForEach(coordinator.workspaces) { workspace in
                        Button { coordinator.browse(workspace) } label: {
                            HStack(spacing: 10) {
                                Label(workspace.name, systemImage: "externaldrive")
                                    .foregroundStyle(.primary)
                                Spacer(minLength: 0)
                                Image(systemName: "chevron.right")
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(.tertiary)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("files.workspace." + workspace.workspaceID)
                    }
                }
            }
            if coordinator.isLoading { ProgressView("work.loading") }
            if let failure = coordinator.failure ?? coordinator.browserFailure {
                Text(LocalizedStringKey(failure.key)).foregroundStyle(.secondary)
                Button("work.retry") { coordinator.retryBrowser() }
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
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: entry.isDirectory ? "folder.fill" : "doc.text")
                .foregroundStyle(entry.isDirectory ? Color.accentColor : .secondary)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 3) {
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
            HStack(spacing: 6) {
                Button {
                    coordinator.selectAgent(coordinator.selectedAgentID)
                } label: {
                    Label("files.workspaces", systemImage: "externaldrive")
                }
                Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.tertiary)
                let parts = directory.path.split(separator: "/").map(String.init)
                if parts.isEmpty {
                    Text(verbatim: directory.workspace.name).fontWeight(.medium).foregroundStyle(.primary)
                } else {
                    Button(directory.workspace.name) { coordinator.browse(directory.workspace) }
                    ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                        Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.tertiary)
                        if index == parts.count - 1 {
                            Text(verbatim: part).fontWeight(.medium).foregroundStyle(.primary)
                        } else {
                            Button(part) { coordinator.browse(directory.workspace, path: parts.prefix(index + 1).joined(separator: "/")) }
                        }
                    }
                }
            }
            .font(.caption)
            .padding(.vertical, 2)
        }.scrollIndicators(.hidden)
    }
}
