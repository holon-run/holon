import CoreData
import Foundation
import HolonClient

/// One transaction covers the complete draft/outbox snapshot; no credentials are encoded.
@MainActor
final class SendingStore {
    private struct Snapshot: Codable {
        var drafts: [SendingScope: SendingDraft] = [:]
        var entries: [SendingEntry] = []
    }
    private let context: NSManagedObjectContext
    private let directory: URL
    private var snapshot: Snapshot
    let maximumAttachmentBytes: Int64
    // Test hook exercises the same rollback path as a failed persistent-store save.
    var beforeSave: (() throws -> Void)?

    init(directory: URL, maximumAttachmentBytes: Int64 = 20 * 1024 * 1024) throws {
        self.directory = directory
        self.maximumAttachmentBytes = maximumAttachmentBytes
        snapshot = Snapshot()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: directory.appendingPathComponent("attachments"),
                                               withIntermediateDirectories: true)
        let model = NSManagedObjectModel()
        let entity = NSEntityDescription()
        entity.name = "SendingSnapshot"
        entity.managedObjectClassName = "NSManagedObject"
        let data = NSAttributeDescription()
        data.name = "data"
        data.attributeType = .binaryDataAttributeType
        data.isOptional = false
        entity.properties = [data]
        model.entities = [entity]
        let coordinator = NSPersistentStoreCoordinator(managedObjectModel: model)
        try coordinator.addPersistentStore(ofType: NSSQLiteStoreType, configurationName: nil,
                                           at: directory.appendingPathComponent("sending.sqlite"), options: nil)
        context = NSManagedObjectContext(concurrencyType: .mainQueueConcurrencyType)
        context.persistentStoreCoordinator = coordinator
        let request = NSFetchRequest<NSManagedObject>(entityName: "SendingSnapshot")
        if let row = try context.fetch(request).first, let data = row.value(forKey: "data") as? Data {
            snapshot = try JSONDecoder().decode(Snapshot.self, from: data)
        }
        var recovered = snapshot
        for index in recovered.entries.indices where recovered.entries[index].state == .sending {
            recovered.entries[index].state = .unknown
            recovered.entries[index].error = "Sending interrupted; receipt unknown. Retry explicitly."
        }
        try commit(recovered)
    }

    // Tests must detach SQLite stores before deleting their temporary directory.
    func closeForTesting() throws {
        context.reset()
        guard let coordinator = context.persistentStoreCoordinator else { return }
        for store in coordinator.persistentStores {
            try coordinator.remove(store)
        }
        context.persistentStoreCoordinator = nil
    }

    func draft(_ scope: SendingScope) -> SendingDraft { snapshot.drafts[scope] ?? SendingDraft() }
    func entries(_ scope: SendingScope) -> [SendingEntry] { snapshot.entries.filter { $0.scope == scope } }
    func saveDraft(_ draft: SendingDraft, scope: SendingScope) throws {
        var next = snapshot
        next.drafts[scope] = draft
        try commit(next)
    }

    func attachmentURL(_ attachment: SendingAttachment) -> URL {
        directory.appendingPathComponent("attachments").appendingPathComponent(attachment.id.uuidString)
    }

    func validate(_ attachment: SendingAttachment) throws {
        let url = attachmentURL(attachment)
        guard FileManager.default.fileExists(atPath: url.path) else {
            throw SendingFailure.missingAttachment(attachment.name)
        }
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard Int64(size) <= maximumAttachmentBytes else {
            throw SendingFailure.oversizedAttachment(attachment.name)
        }
    }

    func stage(source: URL, scope: SendingScope, contentType: String = "application/octet-stream") throws {
        let accessible = source.startAccessingSecurityScopedResource()
        defer { if accessible { source.stopAccessingSecurityScopedResource() } }
        guard FileManager.default.fileExists(atPath: source.path) else {
            throw SendingFailure.missingAttachment(source.lastPathComponent)
        }
        let size = Int64(try source.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0)
        guard size <= maximumAttachmentBytes else {
            throw SendingFailure.oversizedAttachment(source.lastPathComponent)
        }
        let attachment = SendingAttachment(id: UUID(), name: source.lastPathComponent,
                                           byteCount: size, contentType: contentType)
        let target = attachmentURL(attachment)
        try FileManager.default.copyItem(at: source, to: target)
        do {
            try validate(attachment)
            var draft = draft(scope)
            draft.attachments.append(attachment)
            try saveDraft(draft, scope: scope)
        } catch {
            try? FileManager.default.removeItem(at: target)
            throw error
        }
    }

    @discardableResult
    func enqueue(_ scope: SendingScope) throws -> SendingEntry {
        let draft = draft(scope)
        guard !draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !draft.attachments.isEmpty else {
            throw SendingFailure.emptyDraft
        }
        for attachment in draft.attachments { try validate(attachment) }
        let entry = SendingEntry(requestID: UUID(), scope: scope, draft: draft)
        let attachments = try draft.attachments.map { attachment in
            let data = try Data(contentsOf: attachmentURL(attachment))
            do {
                return try HolonPromptAttachment(
                    kind: attachment.contentType.hasPrefix("image/") ? .image : .file,
                    name: attachment.name, mediaType: attachment.contentType, data: data)
            } catch {
                throw SendingFailure.rejected("Invalid prompt or attachment body exceeds the server limit.")
            }
        }
        do {
            _ = try HolonPromptRequest(clientRequestID: entry.requestID.uuidString,
                                       text: draft.text, attachments: attachments)
        } catch {
            throw SendingFailure.rejected("Invalid prompt or attachment body exceeds the server limit.")
        }
        var next = snapshot
        next.entries.append(entry)
        next.drafts[scope] = SendingDraft(modelID: draft.modelID)
        try commit(next)
        return entry
    }

    func update(_ entry: SendingEntry) throws {
        guard let index = snapshot.entries.firstIndex(where: { $0.requestID == entry.requestID }) else { return }
        let old = snapshot.entries[index]
        var receipt = entry
        receipt.canonicalObserved = old.canonicalObserved
        guard old.scope == entry.scope, old.draft == entry.draft,
              old.payload == nil || old.payload == entry.payload,
              old.canonicalObserved != true || entry.canonicalObserved == true,
              old.state != .received || receipt == old else {
            throw SendingFailure.rejected("An immutable queued request cannot be changed.")
        }
        var next = snapshot
        next.entries[index] = entry
        try commit(next)
    }

    /// Explicit imports have their own immutable request and never replace an editor draft.
    @discardableResult
    func enqueueExternal(requestID: UUID, scope: SendingScope, text: String,
                         attachments: [SendingPreparedAttachment], previousOutcomeUnknown: Bool = false) throws -> SendingEntry {
        guard text.utf8.count <= 64 * 1024, attachments.count <= 10,
              attachments.reduce(Int64(0), { $0 + Int64($1.data.count) }) <= maximumAttachmentBytes else {
            throw SendingFailure.rejected("Import exceeds its size limit.")
        }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty else {
            throw SendingFailure.emptyDraft
        }
        let payload = SendingPayload(text: text, modelID: nil, attachments: attachments)
        if let existing = snapshot.entries.first(where: { $0.requestID == requestID }) {
            guard existing.scope == scope, existing.payload == payload else {
                throw SendingFailure.rejected("An import already belongs to another target or payload.")
            }
            return existing
        }
        var copies: [SendingAttachment] = []
        do {
            for source in attachments {
                guard !source.name.isEmpty, source.name.utf8.count <= 255,
                      source.contentType.utf8.count <= 128,
                      !source.contentType.contains("\r"), !source.contentType.contains("\n") else {
                    throw SendingFailure.rejected("Invalid import metadata.")
                }
                let copy = SendingAttachment(id: UUID(), name: source.name,
                                             byteCount: Int64(source.data.count),
                                             contentType: source.contentType)
                copies.append(copy)
                try source.data.write(to: attachmentURL(copy), options: .atomic)
            }
            var entry = SendingEntry(requestID: requestID, scope: scope,
                                     draft: SendingDraft(text: text, attachments: copies))
            entry.payload = payload
            if previousOutcomeUnknown { entry.state = .unknown }
            var next = snapshot
            next.entries.append(entry)
            try commit(next)
            return entry
        } catch {
            for copy in copies { try? FileManager.default.removeItem(at: attachmentURL(copy)) }
            throw error
        }
    }

    func delete(requestID: UUID, scope: SendingScope) throws {
        var next = snapshot
        next.entries.removeAll { $0.requestID == requestID && $0.scope == scope }
        try commit(next)
    }

    private func commit(_ next: Snapshot) throws {
        let removed = attachmentIDs(snapshot).subtracting(attachmentIDs(next))
        do {
            let request = NSFetchRequest<NSManagedObject>(entityName: "SendingSnapshot")
            let row = try context.fetch(request).first ?? NSEntityDescription.insertNewObject(
                forEntityName: "SendingSnapshot", into: context)
            row.setValue(try JSONEncoder().encode(next), forKey: "data")
            try beforeSave?()
            try context.save()
            snapshot = next
        } catch {
            context.rollback()
            throw error
        }
        // Only retired references are candidates, not arbitrary files or in-flight staged copies.
        // The snapshot is already durable: cleanup failure must not masquerade as save failure.
        for id in removed {
            let url = directory.appendingPathComponent("attachments").appendingPathComponent(id.uuidString)
            try? FileManager.default.removeItem(at: url)
        }
    }

    private func attachmentIDs(_ snapshot: Snapshot) -> Set<UUID> {
        let drafts = snapshot.drafts.values.flatMap { $0.attachments }
        let queued = snapshot.entries.flatMap { $0.draft.attachments }
        return Set((drafts + queued).map(\.id))
    }
}
