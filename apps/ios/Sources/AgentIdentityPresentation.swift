import Foundation
import HolonClient

/// The label and expression are one decision, matching the shared Web status precedence.
struct AgentIdentityPresentation: Equatable {
    let statusKey: String
    let expression: HolonAvatarExpression

    /// Roster metadata is authoritative only after its own authenticated refresh.
    static func derive(_ agent: ReadingAgent, status: ReadingStatus, isCached: Bool,
                       isRefreshing: Bool, connecting: Bool = false) -> Self {
        if status == .live, !isCached, !isRefreshing {
            return derive(agent, current: true)
        }
        return .init(statusKey: ReadingPresentation.connectionStatusKey(
            status: status, hasContent: true, isCached: isCached,
            isRefreshing: isRefreshing, connecting: connecting
        ) ?? "avatar.status.unknown", expression: .neutral)
    }

    static func derive(_ agent: ReadingAgent, current: Bool) -> Self {
        guard current else { return .init(statusKey: "avatar.status.offline", expression: .neutral) }
        let lifecycle = normalize(agent.lifecycle), posture = normalize(agent.posture)
        if lifecycle == "deleting" { return .init(statusKey: "avatar.status.deleting", expression: .neutral) }
        if ["stopped", "archived"].contains(lifecycle) || ["stopped", "archived"].contains(posture) {
            return .init(statusKey: "avatar.status.stopped", expression: .neutral)
        }
        if posture == "active-turn" || ["awake-running", "running"].contains(lifecycle)
            || agent.currentRunID?.isEmpty == false {
            return .init(statusKey: "avatar.status.running", expression: .focused)
        }
        if posture == "waiting-for-operator" {
            return .init(statusKey: "avatar.status.needsInput", expression: .questioning)
        }
        if posture.hasPrefix("waiting") || posture == "blocked" || (agent.waitingCount ?? 0) > 0 {
            return .init(statusKey: "avatar.status.waiting", expression: .waiting)
        }
        if ["has-queued-input", "has-runnable-work", "idle"].contains(posture)
            || ["asleep", "awake-idle", "idle", "ready"].contains(lifecycle) || (agent.pendingCount ?? 0) > 0 {
            // The roster has no task count on older servers, as in the shared Web roster.
            if lifecycle == "asleep", posture == "idle", agent.pendingCount == 0,
               (agent.activeTaskCount ?? 0) == 0 {
                return .init(statusKey: "avatar.status.sleeping", expression: .sleeping)
            }
            return .init(statusKey: "avatar.status.ready", expression: .neutral)
        }
        return .init(statusKey: "avatar.status.unknown", expression: .neutral)
    }

    private static func normalize(_ value: String?) -> String {
        (value ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased().split(whereSeparator: { $0 == "_" || $0.isWhitespace }).joined(separator: "-")
    }
}
