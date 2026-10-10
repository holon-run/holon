package run.holon.android.app

import run.holon.android.sdk.HolonContentReportCategory
import run.holon.android.sdk.HolonContentReportRequest
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonHttpException

/** Server-derived target tuple for a content report. `messageId` is the assistant round evidence id. */
internal data class ContentReportTarget(
    val agentId: String,
    val turnId: String,
    val messageId: String,
)

internal const val CONTENT_REPORT_DESCRIPTION_LIMIT = 2_000

private const val ASSISTANT_ACTIVITY_PREFIX = "assistant:"

/**
 * Assistant activities use `assistant:<evidence_id>` ids and only assistant rounds are reportable.
 * Returns null when the activity cannot identify a valid report target.
 */
internal fun contentReportTarget(
    agentId: String?,
    turnId: String?,
    activity: HolonConversationActivity,
): ContentReportTarget? {
    if (activity.kind != "assistant") return null
    val agent = agentId?.takeIf { it.isNotBlank() } ?: return null
    val turn = turnId?.takeIf { it.isNotBlank() } ?: return null
    if (!activity.id.startsWith(ASSISTANT_ACTIVITY_PREFIX)) return null
    val messageId = activity.id.removePrefix(ASSISTANT_ACTIVITY_PREFIX)
    if (messageId.isBlank()) return null
    return ContentReportTarget(agentId = agent, turnId = turn, messageId = messageId)
}

/** Report entry point for one row: null when the activity is not reportable. */
internal fun contentReportAction(
    activity: HolonConversationActivity,
    agentId: String?,
    turnId: String?,
    onReport: (ContentReportTarget) -> Unit,
): (() -> Unit)? =
    contentReportTarget(agentId, turnId, activity)?.let { target ->
        { onReport(target) }
    }

internal data class ContentReportCategoryOption(
    val category: HolonContentReportCategory,
    val sourceLabel: String,
)

internal val contentReportCategoryOptions: List<ContentReportCategoryOption> =
    listOf(
        ContentReportCategoryOption(HolonContentReportCategory.HARMFUL_OR_ABUSIVE, "有害或辱骂内容"),
        ContentReportCategoryOption(HolonContentReportCategory.SEXUAL_CONTENT, "色情或性内容"),
        ContentReportCategoryOption(HolonContentReportCategory.HATE_OR_HARASSMENT, "仇恨或骚扰"),
        ContentReportCategoryOption(HolonContentReportCategory.SELF_HARM, "自残或自杀"),
        ContentReportCategoryOption(HolonContentReportCategory.VIOLENCE, "暴力内容"),
        ContentReportCategoryOption(HolonContentReportCategory.PRIVACY, "隐私泄露"),
        ContentReportCategoryOption(HolonContentReportCategory.SPAM_OR_OTHER, "垃圾信息或其他"),
    )

/** Builds the wire request for a report so the repository stays a thin passthrough. */
internal fun contentReportRequest(
    target: ContentReportTarget,
    category: HolonContentReportCategory,
    description: String?,
    clientRequestId: String,
): HolonContentReportRequest =
    HolonContentReportRequest(
        agentId = target.agentId,
        turnId = target.turnId,
        messageId = target.messageId,
        category = category,
        description = description?.trim()?.takeIf { it.isNotEmpty() },
        clientRequestId = clientRequestId,
    )

internal fun normalizeContentReportDescription(value: String): String =
    value.take(CONTENT_REPORT_DESCRIPTION_LIMIT)

/** Content reports reuse HTTP status codes with report-specific meaning, so map them separately. */
internal fun contentReportError(error: Throwable): String =
    when (error) {
        is HolonHttpException ->
            when (error.statusCode) {
                400 -> "举报信息无效，请重新选择原因后提交"
                404 -> "这条内容已不可举报，可能已被移除"
                429 -> "举报过于频繁，请稍后再试"
                else -> error.apiError?.message ?: humanError(error)
            }
        else -> humanError(error)
    }
