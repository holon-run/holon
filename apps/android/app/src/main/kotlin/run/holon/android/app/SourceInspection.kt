package run.holon.android.app

import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonMessage
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonTurnInput
import run.holon.android.sdk.HolonConversationTurn

internal fun canonicalTriggerInput(turn: HolonConversationTurn): HolonTurnInput? =
    turn.inputs.firstOrNull { !it.interjected }?.takeIf { it.messageId.isNotBlank() }

internal data class SourceInspectionScope(
    val sessionScope: String?,
    val agentId: String,
    val agentIncarnation: Int?,
    val runtimeId: String?,
    val eventLogEpoch: String?,
) {
    fun matches(session: ActiveSession?, agent: AgentSummary?, snapshot: HolonConversationSnapshot?): Boolean =
        session != null && session.scopeKey == sessionScope &&
            agent?.id == agentId && agent.incarnation == agentIncarnation &&
            snapshot?.runtimeId == runtimeId && snapshot?.eventLogEpoch == eventLogEpoch
}

internal enum class SourceFailure { Unauthorized, Forbidden, Missing, Offline, Unavailable }

internal data class SourceInspection(
    val scope: SourceInspectionScope,
    val turnId: String,
    val input: HolonTurnInput,
    val requestId: Long,
    val loading: Boolean = true,
    val message: HolonMessage? = null,
    val failure: SourceFailure? = null,
)
