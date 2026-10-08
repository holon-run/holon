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

internal fun conversationRows(turns: List<HolonConversationTurn>, zone: ZoneId = ZoneId.systemDefault()): List<ConversationRow> = buildList {
    var previousDay: String? = null
    turns.forEach { turn ->
        val day = turn.startedAt?.let { localDate(it, zone) }
        if (day != null && day != previousDay) {
            // Include the turn in the date key: imported history can contain non-monotonic dates.
            add(ConversationRow.Day(day, turn.id))
            previousDay = day
        }
        if (turn.inputs.any { it.presentationClass == "operator" || (it.presentationClass == null && turn.presentationClass == "operator") }) add(ConversationRow.Input(turn))
        add(ConversationRow.Process(turn))
        turn.briefIds.distinct().forEach { add(ConversationRow.Brief(turn, it)) }
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
    data class Interjection(val input: HolonTurnInput) : TurnProcessRow {
        override val key = "input:${input.messageId}"
        override val seq = requireNotNull(input.activityKey).eventSeq
    }
}

/** Placement uses canonical activity identity, not text or wall-clock guesses. */
internal fun turnProcessRows(turn: HolonConversationTurn, activities: List<HolonConversationActivity>): List<TurnProcessRow> =
    (activities.filter { it.kind != "operator" }.map(TurnProcessRow::Activity) +
        turn.inputs.filter { it.interjected && it.activityKey != null && it.presentationClass != "internal" }
            .map(TurnProcessRow::Interjection)).sortedWith(compareBy<TurnProcessRow> { it.seq }.thenBy { it.key })
