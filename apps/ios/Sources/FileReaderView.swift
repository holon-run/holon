import SwiftUI
import UniformTypeIdentifiers

struct FileReaderView: View {
    @Bindable var coordinator: FilesCoordinator
    let request: FilesRequest
    var openFile: (FilesRequest) -> Void
    @Environment(\.scenePhase) private var scenePhase
    @State private var position = FilesReadingPosition()
    @State private var sharing: FileShare?
    @State private var exporting = false
    @State private var exportDocument: FileExportDocument?
    @State private var feedback: String?

    private var artifact: FilesPrepared? { coordinator.preparedRequest == request ? coordinator.prepared : nil }
    var body: some View {
        Group {
            if let artifact {
                FilePreparedReader(artifact: artifact, position: $position, openFile: openFile)
                    .id(artifact.id).accessibilityIdentifier("files.preview")
            } else {
                ContentUnavailableView {
                    Label("files.title", systemImage: "doc")
                } description: {
                    if let failure = coordinator.failure { Text(LocalizedStringKey(failure.key)) }
                    else if coordinator.cancelled { Text("files.cancelled") }
                    else { Text("files.downloading") }
                } actions: {
                    if coordinator.isLoading {
                        if let progress = coordinator.progress {
                            if let total = progress.totalBytes, total > 0 {
                                ProgressView(value: Double(progress.receivedBytes), total: Double(total))
                            } else { ProgressView() }
                            Text(ByteCountFormatter.string(fromByteCount: Int64(progress.receivedBytes), countStyle: .file))
                                .font(.caption).accessibilityIdentifier("files.progress")
                        } else { ProgressView() }
                        Button("action.cancel") { coordinator.cancelDownload() }
                    } else { Button("work.retry") { coordinator.openRequest(request) } }
                }
            }
        }
        .navigationTitle(artifact?.name ?? String(localized: "files.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if let artifact {
                Menu {
                    if artifact.kind == .text {
                        if (artifact.name as NSString).pathExtension.lowercased() == "md", artifact.byteCount <= 256 * 1024 {
                            Toggle("files.markdown", isOn: $position.renderMarkdown)
                        }
                        Toggle("files.wrap", isOn: $position.wrap)
                    }
                    Button("files.export", systemImage: "square.and.arrow.down") { export(artifact) }
                    Button("files.share", systemImage: "square.and.arrow.up") { share(artifact) }
                } label: { Label("files.options", systemImage: "ellipsis") }
                .accessibilityIdentifier("files.options")
            }
        }
        .safeAreaInset(edge: .bottom) {
            if let feedback { Text(LocalizedStringKey(feedback)).font(.caption).foregroundStyle(.secondary).padding(8) }
        }
        .sheet(item: $sharing) { item in
            FileActivityView(url: item.url) { completed in feedback = completed ? "files.shared" : "files.shareCancelled" }
        }
        .fileExporter(isPresented: $exporting, document: exportDocument,
                      contentType: .data, defaultFilename: artifact?.name) { result in
            exportDocument = nil
            switch result {
            case .success: feedback = "files.saved"
            case .failure(let error): feedback = (error as NSError).code == NSUserCancelledError ? "files.shareCancelled" : "files.saveFailed"
            }
        }
        .task(id: request) {
            position = coordinator.readingPosition(for: request)
            if coordinator.preparedRequest != request, coordinator.request != request || !coordinator.isLoading {
                coordinator.openRequest(request)
            }
        }
        .onChange(of: position) { _, value in coordinator.rememberPosition(value, for: request) }
        .onChange(of: coordinator.prepared?.id) { _, _ in clearHandoff() }
        .onChange(of: scenePhase) { _, phase in if phase != .active { clearHandoff() } }
        .onDisappear { clearHandoff() }
    }

    private func clearHandoff() { sharing = nil; exporting = false; exportDocument = nil; feedback = nil }
    private func share(_ artifact: FilesPrepared) {
        Task { @MainActor in
            guard let url = await coordinator.authorizeExport(artifact.id), self.artifact?.id == artifact.id else { return }
            sharing = FileShare(id: artifact.id, url: url)
        }
    }
    private func export(_ artifact: FilesPrepared) {
        Task { @MainActor in
            guard let url = await coordinator.authorizeExport(artifact.id) else { return }
            let data = await Task.detached { try? Data(contentsOf: url) }.value
            guard let data, data.count <= FilesCache.maximumBytes,
                  await coordinator.authorizeExport(artifact.id) != nil, self.artifact?.id == artifact.id else { return }
            exportDocument = FileExportDocument(data: data); exporting = true
        }
    }
}

private struct FilePreparedReader: View {
    let artifact: FilesPrepared
    @Binding var position: FilesReadingPosition
    var openFile: (FilesRequest) -> Void
    @State private var textReader: FileTextReader?
    @State private var rasterReader: FileRasterReader?
    @State private var pageCount = 0
    @State private var failure: FilesFailure?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(verbatim: artifact.mediaType)
                Text(ByteCountFormatter.string(fromByteCount: Int64(artifact.byteCount), countStyle: .file))
                Spacer()
                if let location = artifact.location { Text(verbatim: location.workspaceID).lineLimit(1).truncationMode(.middle) }
            }.font(.caption2).foregroundStyle(.secondary).padding(.horizontal, 16).padding(.vertical, 6)
            if let failure { Text(LocalizedStringKey(failure.key)).foregroundStyle(.secondary).padding(); Spacer() }
            else if let textReader {
                FileTextContent(artifact: artifact, reader: textReader, position: $position, openFile: openFile)
            } else if let rasterReader {
                FileRasterContent(reader: rasterReader, pdf: artifact.kind == .pdf, count: pageCount, position: $position)
            } else if artifact.kind == .downloadOnly {
                ContentUnavailableView("files.preview.downloadOnly", systemImage: "doc")
            } else { ProgressView("work.loading"); Spacer() }
        }
        .task(id: artifact.id) {
            do {
                if artifact.kind == .text {
                    let url = artifact.url
                    let index = try await Task.detached { try FileTextIndex.build(url: url) }.value
                    try Task.checkCancellation()
                    textReader = FileTextReader(url: url, index: index)
                } else if artifact.kind == .image || artifact.kind == .pdf {
                    let reader = FileRasterReader(url: artifact.url)
                    pageCount = artifact.kind == .pdf ? try await reader.pdfPageCount() : 1
                    try Task.checkCancellation()
                    position.page = min(max(0, position.page), pageCount - 1); rasterReader = reader
                }
            } catch {
                if !Task.isCancelled { failure = error as? FilesFailure ?? .unsupported }
            }
        }
    }
}

private struct FileTextContent: View {
    let artifact: FilesPrepared
    let reader: FileTextReader
    @Binding var position: FilesReadingPosition
    var openFile: (FilesRequest) -> Void
    @State private var index: FileTextIndex?
    @State private var visiblePage: Int?
    @State private var query = ""
    @State private var matches: [Int] = []
    @State private var matchNumber = 0
    private var renderMarkdown: Bool {
        position.renderMarkdown && artifact.byteCount <= 256 * 1024 &&
            (artifact.name as NSString).pathExtension.lowercased() == "md"
    }

    var body: some View {
        GeometryReader { geometry in
            VStack(spacing: 0) {
                if !renderMarkdown {
                    HStack {
                        TextField("files.search", text: $query).textInputAutocapitalization(.never).autocorrectionDisabled()
                        if !query.isEmpty {
                            Text("\(matches.isEmpty ? 0 : matchNumber + 1)/\(matches.count)").font(.caption)
                            Button("files.nextMatch", systemImage: "chevron.down") {
                                guard !matches.isEmpty else { return }
                                matchNumber = (matchNumber + 1) % matches.count; visiblePage = matches[matchNumber]
                            }.disabled(matches.isEmpty)
                        }
                        Button("files.end", systemImage: "arrow.down.to.line") { visiblePage = max(0, (index?.pages.count ?? 1) - 1) }
                            .accessibilityIdentifier("files.end")
                    }.padding(12).background(.bar)
                }
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        if renderMarkdown, let text = artifact.text {
                            RichTextContent(text: text,
                                openReference: { openFile(.source(.reference($0))) },
                                openRelative: artifact.location.map { base in { path in openFile(.source(.relative(path, base: base))) } })
                                .id(0).padding(.vertical, 8)
                        } else if let index {
                            if (artifact.name as NSString).pathExtension.lowercased() == "md", artifact.byteCount > 256 * 1024 {
                                Text("files.largeMarkdownSource").font(.caption).foregroundStyle(.secondary).padding(.vertical, 8)
                            }
                            ForEach(index.pages.indices, id: \.self) { page in
                                FileTextPage(reader: reader, page: page, width: max(1, geometry.size.width - 32), wrap: position.wrap,
                                    language: artifact.byteCount <= 1024 * 1024 ? FileCodePresentation.language(name: artifact.name) : nil)
                                    .id(page)
                            }
                            if index.pages.isEmpty { Text("files.emptyText").foregroundStyle(.secondary) }
                        }
                    }.frame(width: max(1, geometry.size.width - 32), alignment: .leading).padding(.horizontal, 16)
                        .scrollTargetLayout()
                }
                .scrollPosition(id: $visiblePage, anchor: .top)
                .textSelection(.enabled)
                HStack {
                    Text(ByteCountFormatter.string(fromByteCount: Int64(artifact.byteCount), countStyle: .file))
                    Spacer()
                    if let index, !renderMarkdown { Text("\(min((visiblePage ?? 0) + 1, index.pages.count))/\(index.pages.count)") }
                }.font(.caption).foregroundStyle(.secondary).padding(8)
            }
        }
        .task { index = reader.index; visiblePage = position.page }
        .task(id: query) {
            matches = []; matchNumber = 0
            guard !query.isEmpty else { return }
            do {
                try await Task.sleep(for: .milliseconds(250))
                let result = try await reader.matches(query)
                try Task.checkCancellation(); matches = result
                if let first = result.first { visiblePage = first }
            } catch {}
        }
        .onChange(of: visiblePage) { _, page in if let page { position.page = page } }
    }
}

private struct FileTextPage: View {
    let reader: FileTextReader
    let page: Int
    let width: CGFloat
    let wrap: Bool
    let language: String?
    @State private var text: String?
    @State private var height: CGFloat = 80
    @State private var failed = false
    var body: some View {
        Group {
            if let text {
                if wrap {
                    content(text).frame(width: width, alignment: .leading)
                } else {
                    ScrollView(.horizontal) { Text(verbatim: text).font(.system(.body, design: .monospaced)).fixedSize(horizontal: true, vertical: false) }
                }
            } else if failed { Text("files.error.unavailable") }
            else { Color.clear.frame(height: height).overlay(ProgressView()) }
        }
        .accessibilityIdentifier("files.text.page.\(page)")
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { if text != nil { height = $0 } }
        .task {
            do { let value = try await reader.page(page); try Task.checkCancellation(); text = value }
            catch { if !Task.isCancelled { failed = true } }
        }
        .onDisappear { text = nil }
    }
    @ViewBuilder private func content(_ text: String) -> some View {
        if let language { RichTextContent(text: FileCodePresentation.markdownSource(text, language: language)) }
        else { Text(verbatim: text).font(.system(.body, design: .monospaced)).frame(maxWidth: .infinity, alignment: .leading) }
    }
}

private struct FileRasterContent: View {
    let reader: FileRasterReader
    let pdf: Bool
    let count: Int
    @Binding var position: FilesReadingPosition
    @State private var image: UIImage?
    @State private var failed = false
    var body: some View {
        VStack {
            if let image { FileZoomImage(image: image) }
            else if failed { ContentUnavailableView("files.preview.imageUnavailable", systemImage: "doc") }
            else { ProgressView("work.loading").frame(maxWidth: .infinity, maxHeight: .infinity) }
            if pdf {
                HStack {
                    Button("files.previousPage", systemImage: "chevron.left") { position.page -= 1 }.disabled(position.page == 0)
                    Spacer(); Text("\(position.page + 1)/\(count)").monospacedDigit(); Spacer()
                    Button("files.nextPage", systemImage: "chevron.right") { position.page += 1 }.disabled(position.page + 1 >= count)
                        .accessibilityIdentifier("files.nextPage")
                }.padding()
            }
        }
        .task(id: position.page) {
            image = nil; failed = false
            do {
                let data = try await reader.image(page: pdf ? position.page : nil)
                try Task.checkCancellation()
                image = UIImage(data: data); failed = image == nil
            } catch { if !Task.isCancelled { failed = true } }
        }
    }
}

private struct FileShare: Identifiable { let id: UUID; let url: URL }
private struct FileActivityView: UIViewControllerRepresentable {
    let url: URL
    var completion: (Bool) -> Void
    func makeUIViewController(context: Context) -> UIActivityViewController {
        let controller = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        controller.completionWithItemsHandler = { _, completed, _, _ in completion(completed) }
        return controller
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
private struct FileExportDocument: FileDocument {
    static let readableContentTypes: [UTType] = [.data]
    let data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}
