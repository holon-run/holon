package run.holon.android.app

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonTurnInput
import run.holon.android.sdk.HolonPendingInput

internal data class PendingConversationInputs(
    val operator: List<HolonPendingInput>,
    val background: List<HolonPendingInput>,
) {
    val keys: List<String>
        get() = operator.map { "pending:${it.messageId}" } +
            if (background.isEmpty()) emptyList() else listOf("pending-background")
}

internal fun pendingConversationInputs(inputs: List<HolonPendingInput>): PendingConversationInputs {
    val (operator, background) = inputs
        .sortedWith(compareBy({ it.createdAt.orEmpty() }, { it.messageId }))
        .partition { it.presentationClass == "operator" }
    return PendingConversationInputs(operator, background)
}

internal sealed interface ConversationRow {
    val key: String
    data class Day(val date: String, val turnId: String) : ConversationRow { override val key = "day:$turnId:$date" }
    data class Input(val turn: HolonConversationTurn) : ConversationRow { override val key = "input:${turn.id}" }
    data class Process(val turn: HolonConversationTurn) : ConversationRow { override val key = "process:${turn.id}" }
    data class Brief(val turn: HolonConversationTurn, val id: String) : ConversationRow { override val key = "brief:${turn.id}:$id" }
}

internal fun conversationRows(turns: List<HolonConversationTurn>, zone: ZoneId = ZoneId.systemDefault(), briefs: Map<String, run.holon.android.sdk.HolonBrief> = emptyMap()): List<ConversationRow> = buildList {
    var previousDay: String? = null
    turns.forEach { turn ->
        val day = turn.startedAt?.let { localDate(it, zone) }
        if (day != null && day != previousDay) {
            // Include the turn in the date key: imported history can contain non-monotonic dates.
            add(ConversationRow.Day(day, turn.id))
            previousDay = day
        }
        if (turn.inputs.any { !it.interjected && (it.presentationClass == "operator" || (it.presentationClass == null && turn.presentationClass == "operator")) }) add(ConversationRow.Input(turn))
        add(ConversationRow.Process(turn))
        turn.briefIds.distinct().filter { id -> briefs[id]?.let { !turn.isRuntimeTaskBrief(it) } ?: true }
            .forEach { add(ConversationRow.Brief(turn, it)) }
    }
}

internal fun localDate(timestamp: String, zone: ZoneId = ZoneId.systemDefault()): String? =
    runCatching { Instant.parse(timestamp).atZone(zone).toLocalDate().toString() }.getOrNull()

internal fun localTimestamp(timestamp: String, zone: ZoneId = ZoneId.systemDefault()): String =
    runCatching { DateTimeFormatter.ofPattern("MM-dd HH:mm").format(Instant.parse(timestamp).atZone(zone)) }.getOrDefault(timestamp)

internal fun readingAnchorIndex(keys: List<String>, anchor: String, fallback: Int): Int =
    keys.indexOf(anchor).takeIf { it >= 0 } ?: fallback.coerceIn(0, keys.lastIndex.coerceAtLeast(0))

internal sealed interface TurnProcessRow {
    val key: String
    val seq: Long
    data class Activity(val activity: HolonConversationActivity) : TurnProcessRow {
        override val key = "activity:${activity.id}"
        override val seq = activity.eventSeq ?: Long.MAX_VALUE
    }
    data class TaskResult(val input: HolonTurnInput) : TurnProcessRow {
        override val key = "input:${input.messageId}"
        override val seq = if (input.interjected) input.activityKey?.eventSeq ?: Long.MAX_VALUE else Long.MIN_VALUE
    }
    data class Interjection(val input: HolonTurnInput) : TurnProcessRow {
        override val key = "input:${input.messageId}"
        override val seq = requireNotNull(input.activityKey).eventSeq
    }
}

/** Placement uses canonical activity identity, not text or wall-clock guesses. */
internal fun turnProcessRows(turn: HolonConversationTurn, activities: List<HolonConversationActivity>): List<TurnProcessRow> =
    turn.inputs.filter { it.taskResult != null && !it.interjected }.map(TurnProcessRow::TaskResult) +
    (activities.filter { it.kind != "operator" }.map(TurnProcessRow::Activity) +
        turn.inputs.filter { it.interjected && it.activityKey != null && it.presentationClass != "internal" && it.taskResult == null }
            .map(TurnProcessRow::Interjection) + turn.inputs.filter { it.taskResult != null && it.interjected }.map(TurnProcessRow::TaskResult)).sortedWith(compareBy<TurnProcessRow> { it.seq }.thenBy { it.key })

internal fun HolonConversationTurn.isRuntimeTaskBrief(brief: run.holon.android.sdk.HolonBrief): Boolean =
    inputs.any { input -> input.taskResult?.let { result ->
        (result.runtimeOnly != false && brief.relatedTaskId == result.taskId) || (result.runtimeOnly == true && brief.relatedMessageId == input.messageId)
    } == true }

internal fun HolonConversationTurn.taskResultHeader(): HolonTurnInput? =
    inputs.firstOrNull { it.taskResult?.status in setOf("failed", "interrupted") }
        ?: inputs.firstOrNull { it.taskResult != null }

/** Prefer the command envelope's cause to its repeated title, without exposing its output path. */
internal fun taskResultFailureReason(preview: String): String {
    val lines = preview.lineSequence().map(String::trim).filter(String::isNotEmpty).toList()
    if (lines.firstOrNull()?.startsWith("command task ") != true) return lines.firstOrNull().orEmpty()
    val outputIndex = lines.indexOf("output_summary:")
    val stderrIndex = lines.indexOf("stderr:")
    val output = if (outputIndex < 0) null else if (stderrIndex > outputIndex) lines.getOrNull(stderrIndex + 1)
        else lines.drop(outputIndex + 1).firstOrNull { it != "stdout:" && it != "stderr:" }
    return lines.firstOrNull { it.startsWith("error:") }
        ?: output
        ?: lines.firstOrNull { it.startsWith("exit_status:") } ?: lines.first()
}

internal fun taskResultPreview(preview: String): String {
    if (!preview.startsWith("command task ")) return preview
    val marker = "\noutput_summary:\n"
    val index = preview.indexOf(marker)
    return if (index >= 0) preview.substring(index + marker.length)
        else preview.lineSequence().firstOrNull { it.startsWith("error:") || it.startsWith("exit_status:") }.orEmpty()
}

internal fun taskResultStatus(result: run.holon.android.sdk.HolonTaskResultPresentation): String =
    if (result.responseMessageId != null && result.status == "completed") ui("收到 Agent 回复") else taskLabel(result.status)
