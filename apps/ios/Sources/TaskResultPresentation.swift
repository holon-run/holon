import Foundation
import HolonClient

/// A process projection over an input; it does not invent a canonical activity.
struct TaskResultInput: Identifiable, Equatable, Sendable {
    let id: String
    let taskID: String
    let status: String
    let summary: String?
    let preview: String
    let responseMessageID: String?
    let runtimeOnly: Bool?
    let createdAt: String?
    let interjected: Bool
    let sequence: Int64?

    init?(_ input: JSONValue) {
        guard let id = input["message_id"]?.readingString, !id.isEmpty,
              let raw = input["task_result"], case .object = raw,
              let taskID = raw["task_id"]?.readingString, !taskID.isEmpty,
              let status = raw["status"]?.readingString,
              ["queued", "running", "cancelling", "completed", "failed", "cancelled", "interrupted"].contains(status),
              let preview = raw["preview"]?.readingString else { return nil }
        if let value = raw["runtime_only"], value != .null, value != .bool(true), value != .bool(false) { return nil }
        for field in ["summary", "response_message_id"] {
            if let value = raw[field], value != .null, value.readingString == nil { return nil }
        }
        self.id = id; self.taskID = taskID; self.status = status
        summary = raw["summary"]?.readingString
        self.preview = preview; responseMessageID = raw["response_message_id"]?.readingString
        runtimeOnly = raw["runtime_only"] == .bool(true) ? true : raw["runtime_only"] == .bool(false) ? false : nil
        createdAt = input["created_at"]?.readingString
        interjected = input["interjected"] == .bool(true)
        sequence = input["activity_key"]?["event_seq"]?.readingInteger
    }

    var isFailure: Bool { status == "failed" || status == "interrupted" }
    var title: String? { summary?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false ? summary : nil }
    var reason: String? {
        guard isFailure, responseMessageID == nil else { return nil }
        let lines = preview.components(separatedBy: .newlines).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        guard lines.first?.hasPrefix("command task ") == true else { return lines.first }
        if let error = lines.first(where: { $0.hasPrefix("error:") }) { return error }
        if let index = lines.firstIndex(of: "output_summary:") {
            if let stderr = lines.firstIndex(of: "stderr:"), stderr > index, lines.indices.contains(stderr + 1) { return lines[stderr + 1] }
            if let output = lines.dropFirst(index + 1).first(where: { $0 != "stdout:" && $0 != "stderr:" }) { return output }
        }
        return lines.first(where: { $0.hasPrefix("exit_status:") }) ?? lines.first
    }
    var displayPreview: String {
        guard preview.hasPrefix("command task ") else { return preview }
        if let range = preview.range(of: "\noutput_summary:\n") { return String(preview[range.upperBound...]) }
        let lines = preview.components(separatedBy: .newlines)
        return lines.first(where: { $0.hasPrefix("error:") })
            ?? lines.first(where: { $0.hasPrefix("exit_status:") }) ?? ""
    }
    var statusKey: String {
        if responseMessageID != nil && status == "completed" { return "reading.replyReceived" }
        return switch status {
        case "queued": "work.state.pending"
        case "running": "work.state.active"
        default: "work.state." + status
        }
    }

    static func items(_ inputs: [JSONValue]) -> [TaskResultInput] { inputs.compactMap(TaskResultInput.init) }
    static func header(_ inputs: [JSONValue]) -> TaskResultInput? {
        let values = items(inputs)
        return values.first(where: \.isFailure) ?? values.first
    }
    static func isRuntimeBrief(_ brief: JSONValue, inputs: [JSONValue]) -> Bool {
        items(inputs).contains { input in
            input.runtimeOnly != false && brief["related_task_id"] == .string(input.taskID)
                || input.runtimeOnly == true && brief["related_message_id"] == .string(input.id)
        }
    }
}

enum ReadingProcessEntry: Identifiable {
    case task(TaskResultInput)
    case activity(ReadingActivity)
    var id: String {
        switch self {
        case .task(let input): "input:" + input.id
        case .activity(let activity): "activity:" + activity.id
        }
    }
    private var sequence: Int64 {
        switch self {
        case .task(let input): input.sequence ?? Int64.max
        case .activity(let activity): activity.sequence
        }
    }
    static func items(inputs: [JSONValue], activities: [ReadingActivity]) -> [ReadingProcessEntry] {
        let results = TaskResultInput.items(inputs)
        let initial = results.filter { !$0.interjected }.map(ReadingProcessEntry.task)
        let ordered = results.filter(\.interjected).map(ReadingProcessEntry.task)
            + activities.filter { $0.kind != "operator" }.map(ReadingProcessEntry.activity)
        return initial + ordered.sorted { $0.sequence == $1.sequence ? $0.id < $1.id : $0.sequence < $1.sequence }
    }
}
