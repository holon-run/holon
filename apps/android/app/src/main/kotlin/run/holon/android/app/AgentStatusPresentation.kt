package run.holon.android.app

import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAvatarExpression

internal enum class AgentStatusKind(
    val key: String,
    val icon: String,
    val toneKey: String,
    val tone: StatusTone,
    val sourceLabel: String,
) {
    Stopped("stopped", "circle-x", "muted", StatusTone.Neutral, "已停止"),
    NeedsInput("needs-input", "circle-alert", "attention", StatusTone.Warning, "需要回应"),
    Running("running", "spinner", "accent", StatusTone.Accent, "运行中"),
    Runnable("runnable", "circle-arrow-right", "accent", StatusTone.Accent, "待运行"),
    Waiting("waiting", "clock", "muted", StatusTone.Neutral, "等待中"),
    Blocked("blocked", "lock", "danger", StatusTone.Danger, "受阻"),
    Ready("ready", "circle-check", "muted", StatusTone.Neutral, "就绪"),
    Unknown("unknown", "circle-help", "muted", StatusTone.Neutral, "未知状态"),
    Deleting("deleting", "trash", "muted", StatusTone.Neutral, "删除中"),
}

internal data class AgentStatusPresentation(val kind: AgentStatusKind) {
    val label: String get() = ui(kind.sourceLabel)
    val expression: HolonAvatarExpression
        get() = when (kind) {
            AgentStatusKind.Running -> HolonAvatarExpression.Focused
            AgentStatusKind.NeedsInput -> HolonAvatarExpression.Questioning
            AgentStatusKind.Waiting, AgentStatusKind.Blocked -> HolonAvatarExpression.Waiting
            else -> HolonAvatarExpression.Neutral
        }
}

/** A known posture is authoritative; summary hints are compatibility fallbacks only. */
internal fun projectAgentStatus(
    lifecycle: String = "",
    posture: String = "",
    currentRunId: String? = null,
    pending: Int = 0,
    waitingCount: Int = 0,
    stateIsCurrent: Boolean = true,
): AgentStatusKind {
    fun String.normalized() = trim().lowercase().replace(Regex("[-\\s]+"), "_")
    val state = lifecycle.normalized()
    val scheduling = posture.normalized()
    if (!stateIsCurrent || state in setOf("offline", "unloaded")) return AgentStatusKind.Unknown
    if (state == "deleting") return AgentStatusKind.Deleting
    if (state in setOf("stopped", "archived", "deleted") || scheduling in setOf("stopped", "archived")) {
        return AgentStatusKind.Stopped
    }
    return when (scheduling) {
        "waiting_for_operator" -> AgentStatusKind.NeedsInput
        "active_turn" -> AgentStatusKind.Running
        "has_queued_input", "has_runnable_work" -> AgentStatusKind.Runnable
        "waiting_for_task", "waiting_for_external" -> AgentStatusKind.Waiting
        "blocked" -> AgentStatusKind.Blocked
        "idle" -> AgentStatusKind.Ready
        else -> when {
            !currentRunId.isNullOrBlank() || state in setOf("running", "awake_running") -> AgentStatusKind.Running
            pending > 0 -> AgentStatusKind.Runnable
            waitingCount > 0 -> AgentStatusKind.Waiting
            state in setOf("idle", "ready", "asleep", "awake_idle") -> AgentStatusKind.Ready
            else -> AgentStatusKind.Unknown
        }
    }
}

internal fun AgentSummary.statusPresentation(live: Boolean = true): AgentStatusPresentation =
    AgentStatusPresentation(
        projectAgentStatus(
            lifecycle = registryStatus.takeIf { it.lowercase() in setOf("deleting", "deleted", "archived", "stopped") }
                ?: runtimeStatus,
            posture = schedulingPosture,
            currentRunId = currentRunId,
            pending = pending,
            waitingCount = if (waitingReason.isNullOrBlank()) 0 else 1,
            stateIsCurrent = live,
        ),
    )
