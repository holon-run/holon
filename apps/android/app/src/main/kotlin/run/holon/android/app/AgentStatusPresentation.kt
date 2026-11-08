package run.holon.android.app

import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAvatarExpression

internal data class AgentStatusPresentation(val label: String, val expression: HolonAvatarExpression)

/** Status text and artwork share the same evidence; stale status never animates a face. */
internal fun AgentSummary.statusPresentation(live: Boolean = true): AgentStatusPresentation {
    val stopped = registryStatus.lowercase() in setOf("deleting", "deleted", "archived", "stopped") ||
        runtimeStatus.lowercase() in setOf("deleting", "deleted", "archived", "stopped", "unloaded", "offline")
    val (label, expression) = when {
        stopped -> (if (runtimeStatus == "offline") ui("离线缓存") else runtimeStatus) to HolonAvatarExpression.Neutral
        needsReply() -> ui("等你回应") to HolonAvatarExpression.Questioning
        schedulingPosture == "active_turn" || currentRunId != null ||
            runtimeStatus.lowercase() in setOf("running", "awake_running") ->
            ui("工作中") to HolonAvatarExpression.Focused
        schedulingPosture == "has_queued_input" -> ui("已排队") to HolonAvatarExpression.Neutral
        schedulingPosture == "has_runnable_work" -> ui("待运行") to HolonAvatarExpression.Neutral
        schedulingPosture == "waiting_for_external" -> ui("等外部变化") to HolonAvatarExpression.Waiting
        schedulingPosture == "waiting_for_task" -> ui("等任务结果") to HolonAvatarExpression.Waiting
        schedulingPosture == "blocked" -> ui("受阻") to HolonAvatarExpression.Waiting
        // The Android roster has no activeTaskCount/waitingCount: idle alone is not sleeping.
        schedulingPosture == "idle" -> ui("空闲") to HolonAvatarExpression.Neutral
        else -> runtimeStatus to HolonAvatarExpression.Neutral
    }
    return AgentStatusPresentation(label, if (live) expression else HolonAvatarExpression.Neutral)
}
