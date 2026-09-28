package run.holon.android.app

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import run.holon.android.sdk.HolonConversationTurn

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
