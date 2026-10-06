import SwiftUI
import UIKit
import UniformTypeIdentifiers
import ImageIO

struct FilesView: View {
    @Bindable var coordinator: FilesCoordinator
    @Environment(\.scenePhase) private var scenePhase
    @State private var reference = ""
    @State private var sharing: FilesShare?
    @State private var exporting = false
    @State private var exportDocument: FilesExportDocument?

    var body: some View {
        NavigationStack {
            Group {
                if let artifact = coordinator.prepared {
                    preview(artifact)
                } else {
                    browser
                }
            }
            .navigationTitle("files.title")
            .overlay {
                if coordinator.isLoading { ProgressView() }
            }
            .safeAreaInset(edge: .bottom) {
                if let failure = coordinator.failure {
                    Text(LocalizedStringKey(failure.key)).foregroundStyle(.red).padding()
                }
            }
        }
        .sheet(item: $sharing) { item in
            FilesActivityView(url: item.url)
        }
        .fileExporter(isPresented: $exporting, document: exportDocument,
                      contentType: .data, defaultFilename: coordinator.prepared?.name) { _ in
            exportDocument = nil
        }
        .onChange(of: coordinator.prepared?.id) { _, _ in
            sharing = nil
            exporting = false
            exportDocument = nil
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active {
                sharing = nil
                exporting = false
                exportDocument = nil
            }
        }
        .onDisappear {
            sharing = nil
            exporting = false
            exportDocument = nil
        }
    }

    private var browser: some View {
        List {
            Section("files.reference") {
                TextField("files.reference.placeholder", text: $reference)
                    .accessibilityIdentifier("files.reference")
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                Button("files.openReference") { coordinator.openReference(reference) }
                    .accessibilityIdentifier("files.openReference")
                    .disabled(reference.isEmpty)
            }
            Section("files.workspaces") {
                ForEach(coordinator.workspaces) { workspace in
                    Button { coordinator.browse(workspace) } label: {
                        Label(workspace.name, systemImage: "externaldrive")
                    }
                }
            }
            if let directory = coordinator.directory {
                Section {
                    Text(directory.path.isEmpty ? "/" : directory.path).font(.caption)
                    if !directory.path.isEmpty {
                        Button("files.parent") {
                            let parent = directory.path.split(separator: "/").dropLast().joined(separator: "/")
                            coordinator.browse(directory.workspace, path: parent)
                        }
                    }
                    Toggle("files.showHidden", isOn: $coordinator.showHidden)
                    TextField("files.filter", text: $coordinator.query)
                    ForEach(coordinator.entries) { entry in
                        Button { coordinator.open(entry) } label: {
                            Label(entry.name, systemImage: entry.isDirectory ? "folder" : "doc")
                        }
                    }
                    if coordinator.entries.isEmpty { Text("files.empty").foregroundStyle(.secondary) }
                } header: { Text("files.directory") }
            }
        }
    }

    private func preview(_ artifact: FilesPrepared) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(artifact.name).font(.headline)
                FilesNativePreview(artifact: artifact)
                    .accessibilityIdentifier("files.preview")
                if artifact.truncated { Text("files.preview.truncated").foregroundStyle(.secondary) }
                HStack {
                    Button("files.export") {
                        Task { @MainActor in
                            guard let url = await coordinator.authorizeExport(artifact.id),
                                  coordinator.prepared?.id == artifact.id,
                                  let data = try? Data(contentsOf: url),
                                  data.count <= FilesCache.maximumBytes else { return }
                            exportDocument = FilesExportDocument(data: data)
                            exporting = true
                        }
                    }
                    Button("files.share") {
                        Task { @MainActor in
                            guard let url = await coordinator.authorizeExport(artifact.id),
                                  coordinator.prepared?.id == artifact.id else { return }
                            sharing = FilesShare(id: artifact.id, url: url)
                        }
                    }
                    Button("files.close") { coordinator.dismissPreview() }
                }
            }.padding()
        }
    }
}

/// Only ImageIO raster thumbnails and literal text. HTML/JS/Markdown never gain execution or URL authority.
private struct FilesNativePreview: View {
    let artifact: FilesPrepared
    @State private var image: UIImage?

    var body: some View {
        Group {
            switch artifact.kind {
            case .text:
                if let text = artifact.text {
                    Text(verbatim: text).font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                } else { Text("files.error.unsupported") }
            case .image:
                if let image {
                    Image(uiImage: image).resizable().scaledToFit()
                } else { Text("files.preview.imageUnavailable") }
            case .downloadOnly:
                Text("files.preview.downloadOnly")
            }
        }
        .task(id: artifact.id) {
            image = nil
            guard artifact.kind == .image,
                  let source = CGImageSourceCreateWithURL(artifact.url as CFURL, nil),
                  let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceThumbnailMaxPixelSize: 1600,
                    kCGImageSourceCreateThumbnailWithTransform: true
                  ] as CFDictionary), !Task.isCancelled else { return }
            image = UIImage(cgImage: thumbnail)
        }
        .id(artifact.id)
    }
}

private struct FilesShare: Identifiable {
    let id: UUID
    let url: URL
}

private struct FilesActivityView: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [url], applicationActivities: nil)
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

private struct FilesExportDocument: FileDocument {
    static let readableContentTypes: [UTType] = [.data]
    let data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws {
        data = configuration.file.regularFileContents ?? Data()
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}
