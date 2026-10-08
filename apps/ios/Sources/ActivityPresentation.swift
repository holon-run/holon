import Foundation
import HolonClient

struct ReadingActivity: Identifiable, Equatable, Sendable {
    let id: String
    let kind: String
    let sequence: Int64
    let revision: Int64
    let summary: String
    let raw: JSONValue

    init(_ raw: JSONValue) throws {
        guard let id = raw["id"]?.readingString, !id.isEmpty, id.utf8.count <= 512,
              let kind = raw["kind"]?.readingString, kind.utf8.count <= 64,
              let sequence = raw["key"]?["event_seq"]?.readingInteger, sequence >= 0,
              raw["key"]?["activity_id"] == .string(id),
              let revision = raw["revision"]?.readingInteger, revision >= 0,
              let summary = raw["summary"]?.readingString else { throw HolonConversationError.malformedProtocol }
        self.id = id; self.kind = kind; self.sequence = sequence; self.revision = revision
        self.summary = summary; self.raw = raw
    }

    var detailID: String? {
        guard kind == "tool" || kind == "assistant", id.hasPrefix(kind + ":") else { return nil }
        let value = String(id.dropFirst(kind.count + 1))
        return value.isEmpty ? nil : value
    }
}

enum ActivityPresentation {
    static func items(_ page: JSONValue?) -> [ReadingActivity] {
        guard case .array(let values) = page?["activities"] else { return [] }
        return values.compactMap { try? ReadingActivity($0) }
    }

    static func validate(_ page: JSONValue, snapshot: HolonConversationSnapshot, turnID: String) throws {
        guard page["runtime_id"] == .string(snapshot.runtimeID),
              page["event_log_epoch"] == .string(snapshot.eventLogEpoch),
              page["visibility_scope_id"] == .string(snapshot.visibilityScopeID),
              let schema = page["schema_version"]?.readingInteger, (1...2).contains(schema),
              let query = page["query_version"]?.readingInteger, (1...2).contains(query),
              page["schema_version"] == snapshot.raw["schema_version"],
              page["query_version"] == snapshot.raw["query_version"],
              page["turn"]?["turn_id"] == .string(turnID),
              let revision = page["detail_revision"]?.readingInteger, revision >= 0,
              case .array(let values) = page["activities"], values.count <= 60,
              case .bool(let more) = page["has_more"],
              !more || page["next_before_cursor"]?.readingString?.isEmpty == false else {
            throw HolonConversationError.bootstrapRequired
        }
        let items = try values.map { try ReadingActivity($0) }
        guard Set(items.map(\.id)).count == items.count,
              (try JSONEncoder().encode(page)).count <= 4_194_304 else {
            throw HolonConversationError.malformedProtocol
        }
    }

    /// Older pages form a moving 180-record window, so every retained activity stays reachable.
    static func merging(_ older: JSONValue, into current: JSONValue, requestedCursor: String) throws -> JSONValue {
        guard current["detail_revision"] == older["detail_revision"],
              current["next_before_cursor"] == .string(requestedCursor),
              older["next_before_cursor"] != .string(requestedCursor),
              case .object(var fields) = older else { throw HolonConversationError.bootstrapRequired }
        var records: [String: ReadingActivity] = [:]
        for item in items(current) + items(older) {
            if let existing = records[item.id] {
                guard existing.sequence == item.sequence, existing.kind == item.kind,
                      existing.revision != item.revision || existing == item else {
                    throw HolonConversationError.malformedProtocol
                }
            }
            if records[item.id].map({ $0.revision > item.revision }) == true { continue }
            records[item.id] = item
        }
        let sorted = records.values.sorted { $0.sequence == $1.sequence ? $0.id < $1.id : $0.sequence < $1.sequence }
        fields["activities"] = .array(sorted.prefix(180).map(\.raw))
        fields["client_window_trimmed"] = .bool(sorted.count > 180 || current["client_window_trimmed"] == .bool(true))
        let merged = JSONValue.object(fields)
        guard (try JSONEncoder().encode(merged)).count <= 4_194_304 else {
            throw HolonConversationError.malformedProtocol
        }
        return merged
    }

    static func assistantText(_ value: JSONValue) -> String {
        let value = decoded(value)
        if let text = value["text"]?.readingString { return text }
        if case .array(let blocks) = value["blocks"] {
            return blocks.filter { $0["type"] == .string("text") }
                .compactMap { $0["text"]?.readingString }.joined(separator: "\n\n")
        }
        return value.readingString ?? ""
    }

    static func decoded(_ value: JSONValue) -> JSONValue {
        guard let text = value.readingString,
              let decoded = try? JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) else { return value }
        return decoded
    }

    static func toolBlocks(_ detail: JSONValue) -> [(key: String, text: String)] {
        var blocks: [(String, String)] = []
        if let raw = detail["input"], raw != .null {
            let input = decoded(raw)
            let command = ["exec_command_display", "cmd_display", "cmd", "command", "command_line"]
                .compactMap { input[$0]?.readingString }.first
            blocks.append((command == nil ? "reading.toolInput" : "reading.command", command ?? display(input)))
        }
        if let raw = detail["output"], raw != .null {
            let outputStart = blocks.count
            var output = decoded(raw)
            for _ in 0..<8 {
                guard let nested = output["envelope"]?["result"] ?? output["result"] else { break }
                output = decoded(nested)
            }
            for (key, candidates) in [("reading.stdout", ["stdout_preview", "stdout", "output_preview"]),
                                      ("reading.stderr", ["stderr_preview", "stderr"]),
                                      ("reading.summary", ["summary_text", "summary"])] {
                if let text = candidates.compactMap({ output[$0]?.readingString }).first { blocks.append((key, text)) }
            }
            if blocks.count == outputStart { blocks.append(("reading.toolOutput", display(output))) }
        }
        if let error = detail["error"], error != .null { blocks.append(("reading.error", display(error))) }
        return blocks
    }

    static func display(_ value: JSONValue) -> String {
        if let text = value.readingString { return text }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return (try? String(decoding: encoder.encode(value), as: UTF8.self)) ?? ""
    }
}
