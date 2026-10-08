import Foundation
import HolonClient

protocol WorkTransport: Sendable {
    func items(agentID: String) async throws -> [WorkRecord]
    func tasks(agentID: String) async throws -> [WorkRecord]
    func items(agentID: String, limit: Int) async throws -> [WorkRecord]
    func tasks(agentID: String, limit: Int) async throws -> [WorkRecord]
    func item(agentID: String, id: String) async throws -> WorkRecord
    func task(agentID: String, id: String) async throws -> WorkRecord
    func output(agentID: String, id: String) async throws -> WorkOutput
    func brief(agentID: String, id: String) async throws -> JSONValue
    func close() async
}

extension WorkTransport {
    func items(agentID: String, limit: Int) async throws -> [WorkRecord] { try await items(agentID: agentID) }
    func tasks(agentID: String, limit: Int) async throws -> [WorkRecord] { try await tasks(agentID: agentID) }
}

/// Owns an independent authenticated client; never binds or mutates UI authority.
actor WorkClientTransport: WorkTransport {
    private let client: HolonClient
    private let authority: HolonConnectionIdentity
    private var bound: HolonConnectionIdentity?
    private var closed = false

    init(client: HolonClient, authority: HolonConnectionIdentity) {
        self.client = client
        self.authority = authority
    }

    private func expected() async throws -> HolonConnectionIdentity {
        guard !closed else { throw CancellationError() }
        let current = await client.identity
        guard !closed, !authority.networkID.isEmpty,
              let runtime = authority.runtimeID, !runtime.isEmpty,
              let user = authority.userID, !user.isEmpty,
              let visibility = authority.visibilityScopeID, !visibility.isEmpty,
              current.networkID == authority.networkID,
              current.runtimeID == runtime, current.userID == user,
              current.visibilityScopeID == visibility,
              bound == nil || current == bound else { throw CancellationError() }
        bound = current
        return current
    }

    private func request<T: Sendable>(
        _ operation: @Sendable (HolonClient) async throws -> HolonResponse<T>
    ) async throws -> T {
        let captured = try await expected()
        do {
            let reply = try await operation(client)
            guard reply.identity == captured, try await expected() == captured else {
                throw CancellationError()
            }
            return reply.value
        } catch let error as HolonHTTPFailure {
            guard error.identity == captured, try await expected() == captured else {
                throw CancellationError()
            }
            throw HolonHTTPFailure(statusCode: error.statusCode, apiError: error.apiError,
                                   identity: authority)
        }
    }

    func items(agentID: String) async throws -> [WorkRecord] {
        try await items(agentID: agentID, limit: 50)
    }
    func items(agentID: String, limit: Int) async throws -> [WorkRecord] {
        guard (1...400).contains(limit) else { throw WorkProtocolError.malformed }
        let raw = try await request {
            try await $0.workItems(agentID: agentID, limit: limit)
        }
        guard let values = raw.workArray ?? raw["items"]?.workArray else {
            throw WorkProtocolError.malformed
        }
        return try records(values, agent: agentID, limit: limit, task: false)
    }

    func tasks(agentID: String) async throws -> [WorkRecord] {
        try await tasks(agentID: agentID, limit: 50)
    }
    func tasks(agentID: String, limit: Int) async throws -> [WorkRecord] {
        guard (1...400).contains(limit) else { throw WorkProtocolError.malformed }
        let raw = try await request {
            try await $0.tasks(agentID: agentID, limit: limit)
        }
        guard let values = raw.workArray ?? raw["tasks"]?.workArray else {
            throw WorkProtocolError.malformed
        }
        return try records(values, agent: agentID, limit: limit, task: true)
    }

    private func records(_ values: [JSONValue], agent: String, limit: Int, task: Bool) throws -> [WorkRecord] {
        guard values.count <= limit, values.allSatisfy({ ownerMatches($0, agent: agent) }) else { throw WorkProtocolError.malformed }
        let result = try values.map { try WorkRecord(raw: $0, task: task) }
        guard Set(result.map(\.id)).count == result.count else { throw WorkProtocolError.malformed }
        return result
    }
    private func ownerMatches(_ raw: JSONValue, agent: String) -> Bool {
        ["agent_id", "owner_agent_id"].allSatisfy { raw[$0] == nil || raw[$0] == .string(agent) }
    }

    func item(agentID: String, id: String) async throws -> WorkRecord {
        let raw = try await request {
            try await $0.workItem(agentID: agentID, workItemID: id)
        }
        let record = try WorkRecord(raw: raw)
        guard record.id == id, ownerMatches(raw, agent: agentID) else { throw WorkProtocolError.malformed }
        return record
    }

    func task(agentID: String, id: String) async throws -> WorkRecord {
        let raw = try await request {
            try await $0.task(agentID: agentID, taskID: id)
        }
        let record = try WorkRecord(raw: raw, task: true)
        guard record.id == id, ownerMatches(raw, agent: agentID) else { throw WorkProtocolError.malformed }
        return record
    }

    func output(agentID: String, id: String) async throws -> WorkOutput {
        let raw = try await request {
            try await $0.taskOutput(agentID: agentID, taskID: id)
        }
        guard (raw["task"] ?? raw)["task_id"]?.workString == id else {
            throw WorkProtocolError.malformed
        }
        return try WorkOutput(raw: raw)
    }

    func brief(agentID: String, id: String) async throws -> JSONValue {
        let raw = try await request { try await $0.briefDetail(agentID: agentID, briefID: id) }
        guard raw["brief_id"] == .string(id) || raw["id"] == .string(id), ownerMatches(raw, agent: agentID) else { throw WorkProtocolError.malformed }
        return raw
    }

    func close() async {
        closed = true
        await client.close()
    }
}
