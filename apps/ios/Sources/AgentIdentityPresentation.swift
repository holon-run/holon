import Foundation
import HolonClient

/// One display decision for roster rows, selected headers, and decorative avatars.
struct AgentIdentityPresentation: Equatable {
    enum Kind: String {
        case stopped, running, runnable, waiting, blocked, ready, unknown, deleting
        case needsInput = "needs-input"
    }

    enum Icon: String {
        case circleX = "circle-x"
        case circleAlert = "circle-alert"
        case spinner
        case circleArrowRight = "circle-arrow-right"
        case clock, lock
        case circleCheck = "circle-check"
        case circleHelp = "circle-help"
        case circleMinus = "circle-minus"

        var systemName: String {
            switch self {
            case .circleX: "xmark.circle"
            case .circleAlert: "exclamationmark.circle"
            case .spinner: "arrow.triangle.2.circlepath"
            case .circleArrowRight: "arrow.right.circle"
            case .clock: "clock"
            case .lock: "lock"
            case .circleCheck: "checkmark.circle"
            case .circleHelp: "questionmark.circle"
            case .circleMinus: "minus.circle"
            }
        }
    }

    enum Tone: String { case muted, attention, accent, danger }

    let kind: Kind
    /// Connection/cache information is secondary, never a replacement scheduling claim.
    var connectionStatusKey: String?

    var statusKey: String {
        "avatar.status." + (kind == .needsInput ? "needsInput" : kind.rawValue)
    }

    var icon: Icon {
        switch kind {
        case .stopped: .circleX
        case .needsInput: .circleAlert
        case .running: .spinner
        case .runnable: .circleArrowRight
        case .waiting: .clock
        case .blocked: .lock
        case .ready: .circleCheck
        case .unknown: .circleHelp
        case .deleting: .circleMinus
        }
    }

    var tone: Tone {
        switch kind {
        case .running, .runnable: .accent
        case .needsInput: .attention
        case .blocked: .danger
        default: .muted
        }
    }

    var expression: HolonAvatarExpression {
        switch kind {
        case .running: .focused
        case .needsInput: .questioning
        case .waiting: .waiting
        default: .neutral
        }
    }

    func animates(reduceMotion: Bool) -> Bool { kind == .running && !reduceMotion }

    /// Roster metadata is authoritative only after its own authenticated refresh.
    static func derive(_ agent: ReadingAgent, status: ReadingStatus, isRosterCurrent: Bool,
                       isCached: Bool, isRefreshing: Bool, connecting: Bool = false) -> Self {
        if status == .live, isRosterCurrent, !isCached, !isRefreshing {
            return derive(agent, current: true)
        }
        return .init(kind: .unknown, connectionStatusKey: ReadingPresentation.connectionStatusKey(
            status: status, hasContent: true, isCached: isCached || !isRosterCurrent,
            isRefreshing: isRefreshing, connecting: connecting
        ))
    }

    static func derive(_ agent: ReadingAgent, current: Bool) -> Self {
        guard current else { return .init(kind: .unknown) }
        let lifecycle = normalize(agent.lifecycle), posture = normalize(agent.posture)
        if lifecycle == "deleting" { return .init(kind: .deleting) }
        if ["stopped", "archived", "deleted"].contains(lifecycle) || ["stopped", "archived"].contains(posture) {
            return .init(kind: .stopped)
        }

        // Known posture is authoritative over compatibility counters and stale run IDs.
        switch posture {
        case "waiting-for-operator": return .init(kind: .needsInput)
        case "active-turn": return .init(kind: .running)
        case "has-queued-input", "has-runnable-work": return .init(kind: .runnable)
        case "waiting-for-task", "waiting-for-external": return .init(kind: .waiting)
        case "blocked": return .init(kind: .blocked)
        case "idle": return .init(kind: .ready)
        default: break
        }
        if ["awake-running", "running"].contains(lifecycle) || agent.currentRunID?.isEmpty == false {
            return .init(kind: .running)
        }
        if (agent.pendingCount ?? 0) > 0 { return .init(kind: .runnable) }
        if (agent.waitingCount ?? 0) > 0 { return .init(kind: .waiting) }
        if ["asleep", "awake-idle", "idle", "ready"].contains(lifecycle) { return .init(kind: .ready) }
        // Background task counts alone prove neither an active turn nor an explicit wait.
        return .init(kind: .unknown)
    }

    private static func normalize(_ value: String?) -> String {
        (value ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased().split(whereSeparator: { $0 == "_" || $0.isWhitespace }).joined(separator: "-")
    }
}
