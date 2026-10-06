import Foundation
import HolonClient

enum WorkLoadState: Equatable {
    case idle, loading, loaded, failed, disconnected, offline, incompatible
}

enum WorkRoute: Hashable {
    case item(String), task(String), brief(String)
}

struct WorkRecord: Identifiable, Equatable, Sendable {
    let id: String
    let raw: JSONValue
    var title: String { raw["objective"]?.workString ?? raw["summary"]?.workString ?? id }
    var state: String { raw["state"]?.workString ?? raw["status"]?.workString ?? "unknown" }
    var plan: JSONValue? {
        guard case .object = raw["plan_artifact"] else { return nil }
        return raw["plan_artifact"]
    }
    var briefID: String? { raw["result_brief_id"]?.workString }
    var references: [JSONValue] { raw["work_refs"]?.workArray ?? [] }

    init(raw: JSONValue, task: Bool = false) throws {
        guard case .object = raw,
              let id = raw[task ? "task_id" : "id"]?.workString
                ?? raw[task ? "id" : "work_item_id"]?.workString,
              !id.isEmpty, id.utf8.count <= 512 else { throw WorkProtocolError.malformed }
        self.id = id
        self.raw = raw
    }
}

struct WorkOutput: Equatable, Sendable {
    let text: String?
    let status: String
    let truncated: Bool

    init(raw: JSONValue) throws {
        let value = raw["task"] ?? raw
        guard case .object = value else { throw WorkProtocolError.malformed }
        let original = value["output_preview"]?.workString ?? value["result_summary"]?.workString
        text = original.map { String($0.prefix(32_768)) }
        status = value["status"]?.workString ?? "unknown"
        truncated = value["output_truncated"] == .bool(true) || (original?.count ?? 0) > 32_768
    }
}

enum WorkProtocolError: Error { case malformed }

extension JSONValue {
    var workString: String? {
        guard case .string(let value) = self else { return nil }
        return value
    }
    var workArray: [JSONValue]? {
        guard case .array(let value) = self else { return nil }
        return value
    }
    var workDisplay: String {
        if let value = workString { return value }
        guard let data = try? JSONEncoder().encode(self) else { return "" }
        return String(decoding: data, as: UTF8.self)
    }
}
