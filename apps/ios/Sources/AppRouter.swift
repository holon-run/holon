import Foundation
import Observation

enum AppRoute: Hashable {
    case conversation(String), work(String), workDetail(String, WorkRoute), files(String), file(String, FilesRequest)
    case settings, connections, tools, diagnostics

    var agentID: String? {
        switch self {
        case .conversation(let id), .work(let id), .workDetail(let id, _), .files(let id), .file(let id, _): id
        default: nil
        }
    }
}

/// Selection owns the Agent's coordinators; a child route never changes that selection.
@MainActor @Observable
final class AppRouter {
    var path: [AppRoute] = []
    private(set) var partition: ReadingPartition?
    @ObservationIgnored private let preferences: UserDefaults?
    @ObservationIgnored private var restoringAgent: String?
    private struct Saved: Codable { let partition: ReadingPartition; let agentID: String }
    private let key = "navigation.confirmedAgent.v1"

    init(preferences: UserDefaults? = .standard) { self.preferences = preferences }
    var agentID: String? { path.compactMap(\.agentID).first }

    func activate(_ partition: ReadingPartition?) {
        path = []
        restoringAgent = nil
        if partition == nil { preferences?.removeObject(forKey: key) }
        self.partition = partition
        guard let partition, let data = preferences?.data(forKey: key), data.count <= 4096,
              let saved = try? JSONDecoder().decode(Saved.self, from: data),
              saved.partition == partition, !saved.agentID.isEmpty,
              saved.agentID.utf8.count <= 512 else { return }
        restoringAgent = saved.agentID
    }

    func restore(agents: [ReadingAgent], authoritative: Bool) {
        guard authoritative, path.isEmpty, let id = restoringAgent else { return }
        restoringAgent = nil
        if agents.contains(where: { $0.id == id }) { path = [.conversation(id)] }
    }

    func remember() {
        guard let partition, let agentID else {
            if restoringAgent == nil { preferences?.removeObject(forKey: key) }
            return
        }
        let saved = Saved(partition: partition, agentID: agentID)
        if let data = try? JSONEncoder().encode(saved), data.count <= 4096 { preferences?.set(data, forKey: key) }
    }

    func withdrawAgent() {
        path.removeAll { $0.agentID != nil }
        restoringAgent = nil
        preferences?.removeObject(forKey: key)
    }
}
