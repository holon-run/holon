package run.holon.android.app

import java.time.ZoneId
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonObject
import run.holon.android.sdk.HolonActivityKey
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonTaskResultPresentation
import run.holon.android.sdk.HolonTurnInput

class ConversationPresentationRulesTest {
    @Test fun `boundary follows day and precedes input without changing existing keys`() {
        val turn = completedTurn().copy(
            presentationClass = "operator",
            inputs = listOf(input().copy(presentationClass = "operator", taskResult = null)),
            briefIds = listOf("reply", "reply"),
            resultKind = "available",
        )
        val rows = conversationRows(listOf(turn), zone, expandedTurnId = turn.id)
        assertEquals(
            listOf("day:turn:2026-10-20", "boundary:turn", "input:turn", "process:turn", "brief:turn:reply"),
            rows.map { it.key },
        )
        assertEquals(turn, rows.filterIsInstance<ConversationRow.Boundary>().single().turn)
        val later = conversationRows(listOf(completedTurn("older"), turn), zone, expandedTurnId = turn.id)
        val stableKeys = rows.filterNot { it is ConversationRow.Day }.map { it.key }
        assertEquals(stableKeys, later.filter { it.key in stableKeys }.map { it.key })
    }

    @Test fun `only a fully evidenced quiet reducer receipt is omitted`() {
        assertEquals(emptyList(), conversationRows(listOf(receipt()), zone))
        val legacy = receipt().copy(inputs = listOf(input().copy(taskResult = result().copy(runtimeOnly = null))))
        assertEquals(emptyList(), conversationRows(listOf(legacy), zone))
    }

    @Test fun `every reducer condition must hold before hiding a receipt`() {
        val receipt = receipt()
        val cases = listOf(
            "operator source" to receipt.copy(presentationClass = "operator"),
            "missing source" to receipt.copy(presentationClass = null),
            "unknown source" to receipt.copy(presentationClass = "future"),
            "active" to receipt.copy(executionKind = "active"),
            "missing execution" to receipt.copy(executionKind = "unknown"),
            "missing outcome" to receipt.copy(terminalOutcome = null),
            "aborted" to receipt.copy(terminalOutcome = "aborted"),
            "interrupted" to receipt.copy(terminalOutcome = "interrupted"),
            "over budget" to receipt.copy(terminalOutcome = "baseline_over_budget"),
            "fallback" to receipt.copy(terminalOutcome = "deferred_to_fallback"),
            "provider failure" to receipt.copy(terminalOutcome = "provider_failed_needs_recovery"),
            "unsettled" to receipt.copy(settled = false),
            "attention" to receipt.copy(attentionKind = "waiting"),
            "unknown attention" to receipt.copy(attentionKind = "future"),
            "brief" to receipt.copy(briefIds = listOf("reply")),
            "pending result" to receipt.copy(resultKind = "pending"),
            "available result" to receipt.copy(resultKind = "available"),
            "unavailable result" to receipt.copy(resultKind = "unavailable"),
            "missing result" to receipt.copy(resultKind = "unknown"),
            "missing reason" to receipt.copy(resultReasonKind = null),
            "other reason" to receipt.copy(resultReasonKind = "tool_only_wait"),
            "unknown reason" to receipt.copy(resultReasonKind = "future"),
            "truncated inputs" to receipt.copy(inputsTruncated = true),
            "missing inputs" to receipt.copy(inputs = emptyList()),
            "missing task metadata" to receipt.copy(inputs = listOf(input().copy(taskResult = null))),
        )
        cases.forEach { (name, turn) ->
            assertEquals(listOf(turn.id), boundaries(conversationRows(listOf(turn), zone)), name)
        }
    }

    @Test fun `failed cancelled interrupted and unfinished tasks remain visible`() {
        listOf("queued", "running", "cancelling", "failed", "cancelled", "interrupted", "future").forEach { status ->
            val turn = receipt().copy(inputs = listOf(input().copy(taskResult = result().copy(status = status))))
            assertEquals(listOf(turn.id), boundaries(conversationRows(listOf(turn), zone)), status)
        }
    }

    @Test fun `original reply links and explicit non-runtime provenance survive filtering`() {
        val linked = receipt().copy(inputs = listOf(input().copy(taskResult = result().copy(responseMessageId = "original"))))
        val reply = receipt().copy(inputs = listOf(input().copy(taskResult = result().copy(runtimeOnly = false))))
        listOf(linked, reply).forEach { turn ->
            assertEquals(listOf(turn.id), boundaries(conversationRows(listOf(turn), zone)))
        }
    }

    @Test fun `all inputs must be harmless before filtering a multi-input receipt`() {
        val receipt = receipt()
        val second = input().copy(messageId = "second")
        assertEquals(emptyList(), conversationRows(listOf(receipt.copy(inputs = receipt.inputs + second)), zone))
        listOf(
            second.copy(taskResult = result().copy(status = "failed")),
            second.copy(taskResult = result().copy(responseMessageId = "original")),
            second.copy(taskResult = result().copy(runtimeOnly = false)),
            second.copy(taskResult = null, presentationClass = "operator", interjected = true),
        ).forEach { meaningful ->
            val turn = receipt.copy(inputs = receipt.inputs + meaningful)
            assertEquals(listOf(turn.id), boundaries(conversationRows(listOf(turn), zone)))
        }
    }

    @Test fun `reveal retains the canonical receipt independently from process expansion`() {
        val receipt = receipt()
        val rows = conversationRows(listOf(receipt), zone, revealTurnIds = setOf(receipt.id))
        assertEquals(listOf("day:receipt:2026-10-20", "boundary:receipt"), rows.map { it.key })
        val expanded = conversationRows(
            listOf(receipt), zone, revealTurnIds = setOf(receipt.id), expandedTurnId = receipt.id,
        )
        assertEquals(rows.map { it.key } + "process:receipt", expanded.map { it.key })
        assertEquals(emptyList(), conversationRows(listOf(receipt), zone, expandedTurnId = receipt.id))
        assertEquals(emptyList(), conversationRows(listOf(receipt), zone, revealTurnIds = setOf("other")))
        assertEquals(emptyList(), conversationRows(listOf(receipt), zone))
    }

    @Test fun `filtering happens before date generation without consuming a visible day anchor`() {
        val hidden = receipt()
        val visible = completedTurn()
        assertEquals(
            conversationRows(listOf(visible), zone).map { it.key },
            conversationRows(listOf(hidden, visible), zone).map { it.key },
        )
        val hiddenTomorrow = hidden.copy(startedAt = "2026-10-21T08:00:00Z")
        val rows = conversationRows(listOf(visible, hiddenTomorrow), zone)
        assertEquals(listOf("2026-10-20"), rows.filterIsInstance<ConversationRow.Day>().map { it.date })
    }

    @Test fun `date grouping uses visible local dates and retains unique keys for imported order`() {
        val first = completedTurn("first").copy(startedAt = "2026-10-20T23:30:00Z")
        val middle = completedTurn("middle").copy(startedAt = "2026-10-21T23:30:00Z")
        val last = first.copy(id = "last")
        val rows = conversationRows(listOf(first, middle, last), ZoneId.of("Asia/Shanghai"))
        assertEquals(listOf("2026-10-21", "2026-10-22", "2026-10-21"), rows.filterIsInstance<ConversationRow.Day>().map { it.date })
        assertEquals(rows.size, rows.map { it.key }.distinct().size)
        assertFalse(conversationRows(listOf(first.copy(startedAt = "invalid")), zone).any { it is ConversationRow.Day })
    }

    @Test fun `empty actually executed turns stay distinct even when they have no reply`() {
        val turns = listOf(
            completedTurn("internal"),
            completedTurn("task").copy(presentationClass = "task"),
            completedTurn("legacy").copy(presentationClass = null, resultReasonKind = null),
        )
        val rows = conversationRows(turns, zone)
        assertEquals(turns.map { it.id }, boundaries(rows))
        assertFalse(rows.any { it is ConversationRow.Process })
        assertFalse(rows.any { it is ConversationRow.Input || it is ConversationRow.Brief })
    }

    @Test fun `only the expanded turn receives a process row without dropping boundaries`() {
        val turns = listOf(completedTurn("first"), completedTurn("second"), completedTurn("third"))
        turns.forEach { selected ->
            val rows = conversationRows(turns, zone, expandedTurnId = selected.id)
            assertEquals(turns.map { it.id }, boundaries(rows))
            assertEquals(listOf("process:${selected.id}"), rows.filterIsInstance<ConversationRow.Process>().map { it.key })
        }
        assertFalse(conversationRows(turns, zone, expandedTurnId = "absent").any { it is ConversationRow.Process })
    }

    @Test fun `collapsed turns retain canonically placed noninternal interjections`() {
        listOf("operator", "external", "task", null).forEach { presentation ->
            val interjection = input().copy(
                messageId = "interjection",
                presentationClass = presentation,
                taskResult = null,
                interjected = true,
                activityKey = HolonActivityKey(12, "operator:interjection"),
            )
            val turn = completedTurn().copy(inputs = listOf(interjection))
            val rows = conversationRows(listOf(turn), zone)
            assertEquals(listOf("process:turn"), rows.filterIsInstance<ConversationRow.Process>().map { it.key })
            assertEquals(listOf("input:interjection"), turnProcessRows(turn, emptyList()).map { it.key })
        }
    }

    @Test fun `collapsed turns retain legacy operator interjections without activity keys`() {
        val interjection = input().copy(
            messageId = "legacy-interjection",
            presentationClass = "operator",
            taskResult = null,
            interjected = true,
            activityKey = null,
        )
        val turn = completedTurn().copy(inputs = listOf(interjection))
        val rows = conversationRows(listOf(turn), zone)
        assertEquals(listOf(turn.id), boundaries(rows))
        assertEquals(listOf("process:turn"), rows.filterIsInstance<ConversationRow.Process>().map { it.key })
    }

    @Test fun `internal noninterjected and task result inputs do not add collapsed process rows`() {
        val interjection = input().copy(
            presentationClass = "operator",
            taskResult = null,
            interjected = true,
            activityKey = HolonActivityKey(12, "operator:message"),
        )
        val hidden = listOf(
            interjection.copy(presentationClass = "internal"),
            interjection.copy(interjected = false),
            interjection.copy(taskResult = result()),
            input(),
        )
        hidden.forEach { candidate ->
            val turn = completedTurn().copy(inputs = listOf(candidate))
            val rows = conversationRows(listOf(turn), zone)
            assertEquals(listOf(turn.id), boundaries(rows))
            assertFalse(rows.any { it is ConversationRow.Process })
        }
    }

    @Test fun `terminal outcomes use exact wire kinds and never invent a success notice`() {
        val expectations = mapOf(
            "completed" to emptyList(),
            "aborted" to listOf(TurnNotice.Interrupted),
            "interrupted" to listOf(TurnNotice.Interrupted),
            "baseline_over_budget" to listOf(TurnNotice.Failed),
            "deferred_to_fallback" to listOf(TurnNotice.Syncing),
            "provider_failed_needs_recovery" to listOf(TurnNotice.Failed),
            "future" to listOf(TurnNotice.Syncing),
        )
        expectations.forEach { (outcome, notices) ->
            assertEquals(notices, turnNotices(completedTurn().copy(terminalOutcome = outcome)), outcome)
        }
        assertEquals(listOf(TurnNotice.Syncing), turnNotices(completedTurn().copy(terminalOutcome = null)))
        assertEquals(emptyList(), turnNotices(receipt()))
    }

    @Test fun `turn attention remains visible independently from result presence`() {
        val turn = completedTurn().copy(resultKind = "available", briefIds = listOf("not-loaded"))
        mapOf(
            "failed" to TurnNotice.Failed,
            "interrupted" to TurnNotice.Interrupted,
            "waiting" to TurnNotice.Waiting,
            "future" to TurnNotice.Syncing,
        ).forEach { (attention, notice) ->
            assertEquals(listOf(notice), turnNotices(turn.copy(attentionKind = attention)), attention)
        }
        assertEquals(
            listOf(TurnNotice.Running, TurnNotice.Failed),
            turnNotices(turn.copy(executionKind = "active", completedAt = null, attentionKind = "failed")),
        )
    }

    @Test fun `task outcomes never contaminate a receiving turn status`() {
        listOf("completed", "failed", "cancelled", "interrupted", "running").forEach { status ->
            val turn = completedTurn().copy(inputs = listOf(input().copy(taskResult = result().copy(status = status))))
            assertEquals(emptyList(), turnNotices(turn), status)
            assertEquals(listOf(turn.id), boundaries(conversationRows(listOf(turn), zone)), status)
        }
    }

    @Test fun `running waiting unsettled and unknown projection states remain meaningful`() {
        val turn = completedTurn()
        assertEquals(listOf(TurnNotice.Running), turnNotices(turn.copy(executionKind = "active", completedAt = null, settled = false, resultKind = "pending")))
        assertEquals(listOf(TurnNotice.Waiting), turnNotices(turn.copy(resultKind = "pending")))
        assertEquals(listOf(TurnNotice.Waiting), turnNotices(turn.copy(settled = false)))
        assertEquals(listOf(TurnNotice.Syncing), turnNotices(turn.copy(executionKind = "unknown")))
        assertEquals(listOf(TurnNotice.Syncing), turnNotices(turn.copy(executionKind = "unknown", resultKind = "unknown")))
    }

    @Test fun `completed active snapshots never regain a running notice`() {
        val staleActive = completedTurn().copy(executionKind = "active", settled = false)
        listOf("none", "pending").forEach { resultKind ->
            assertEquals(emptyList(), turnNotices(staleActive.copy(resultKind = resultKind)), resultKind)
        }
        assertEquals(listOf(TurnNotice.Failed), turnNotices(staleActive.copy(attentionKind = "failed")))
        assertEquals(
            listOf(TurnNotice.Running),
            turnNotices(staleActive.copy(completedAt = null, resultKind = "pending")),
        )
    }

    @Test fun `known brief identities are not falsely described as missing while unloaded`() {
        val turn = completedTurn().copy(briefIds = listOf("not-loaded"))
        listOf("available", "unknown", "none").forEach { kind ->
            assertFalse(TurnNotice.MissingResult in turnNotices(turn.copy(resultKind = kind)), kind)
        }
        assertEquals(emptyList(), turnNotices(turn.copy(resultKind = "available")))
        assertEquals(listOf(TurnNotice.Waiting), turnNotices(turn.copy(resultKind = "pending", settled = false)))
        assertEquals(listOf("not-loaded"), conversationRows(listOf(turn), zone).filterIsInstance<ConversationRow.Brief>().map { it.id })
    }

    @Test fun `missing unavailable and truncated data are distinct from a normal no-result turn`() {
        val turn = completedTurn()
        assertEquals(emptyList(), turnNotices(turn))
        assertEquals(listOf(TurnNotice.MissingResult), turnNotices(turn.copy(resultKind = "available")))
        assertEquals(listOf(TurnNotice.MissingResult), turnNotices(turn.copy(resultKind = "unknown")))
        assertEquals(listOf(TurnNotice.ResultUnavailable), turnNotices(turn.copy(resultKind = "unavailable")))
        assertEquals(listOf(TurnNotice.TruncatedInputs), turnNotices(turn.copy(inputsTruncated = true)))
        assertEquals(
            listOf(TurnNotice.Failed, TurnNotice.ResultUnavailable, TurnNotice.TruncatedInputs),
            turnNotices(turn.copy(
                terminalOutcome = "provider_failed_needs_recovery",
                attentionKind = "failed",
                resultKind = "unavailable",
                inputsTruncated = true,
            )),
        )
    }

    @Test fun `runtime task briefs are deduplicated only by canonical task or message identity`() {
        val receipt = receipt().copy(briefIds = listOf("legacy"))
        val brief = HolonBrief("legacy", "agent", null, "result", "", "same text", emptyList(), null, "message")
        assertTrue(receipt.isRuntimeTaskBrief(brief))
        assertTrue(receipt.isRuntimeTaskBrief(brief.copy(relatedMessageId = "other", relatedTaskId = "task")))
        val unrelated = brief.copy(relatedMessageId = "other", relatedTaskId = "other")
        assertFalse(receipt.isRuntimeTaskBrief(unrelated))
        assertFalse(conversationRows(listOf(receipt), zone, mapOf("legacy" to brief)).any { it is ConversationRow.Brief })
        assertTrue(conversationRows(listOf(receipt), zone, mapOf("legacy" to unrelated)).any { it is ConversationRow.Brief })
        val reply = receipt.copy(inputs = listOf(input().copy(taskResult = result().copy(runtimeOnly = false))))
        assertFalse(reply.isRuntimeTaskBrief(brief.copy(relatedTaskId = "task")))
        assertTrue(conversationRows(listOf(reply), zone, mapOf("legacy" to brief)).any { it is ConversationRow.Brief })
    }

    @Test fun `canonical interjection placement is independent from preview text and wall clock`() {
        val interjection = input().copy(
            messageId = "interjection",
            presentationClass = "operator",
            taskResult = null,
            interjected = true,
            activityKey = HolonActivityKey(12, "operator:interjection"),
            createdAt = "1900-01-01T00:00:00Z",
        )
        val turn = completedTurn().copy(inputs = listOf(input(), interjection))
        val activities = listOf(
            HolonConversationActivity("after", "assistant", "same text", 14, 1, buildJsonObject {}),
            HolonConversationActivity("operator:interjection", "operator", "same text", 12, 1, buildJsonObject {}),
            HolonConversationActivity("before", "tool", "same text", 10, 1, buildJsonObject {}),
        )
        assertEquals(
            listOf("input:message", "activity:before", "input:interjection", "activity:after"),
            turnProcessRows(turn, activities).map { it.key },
        )
    }

    @Test fun `duration is compact truncates fractional seconds and rejects invalid values`() {
        assertNull(compactTurnDuration(null))
        assertNull(compactTurnDuration(-1))
        assertNull(compactTurnDuration(Long.MIN_VALUE))
        mapOf(
            0L to "0s",
            999L to "0s",
            1_000L to "1s",
            59_999L to "59s",
            60_000L to "1m0s",
            279_999L to "4m39s",
            3_599_999L to "59m59s",
            3_600_000L to "1h0m0s",
            7_445_000L to "2h4m5s",
        ).forEach { (duration, expected) -> assertEquals(expected, compactTurnDuration(duration)) }
    }

    private val zone = ZoneId.of("UTC")

    private fun boundaries(rows: List<ConversationRow>) =
        rows.filterIsInstance<ConversationRow.Boundary>().map { it.turn.id }

    private fun completedTurn(id: String = "turn") = HolonConversationTurn(
        id = id,
        summary = "",
        presentationClass = "internal",
        inputs = emptyList(),
        executionKind = "terminal",
        terminalOutcome = "completed",
        resultKind = "none",
        attentionKind = null,
        briefIds = emptyList(),
        startedAt = "2026-10-20T08:00:00Z",
        completedAt = "2026-10-20T08:01:00Z",
        settled = true,
        raw = buildJsonObject {},
        resultReasonKind = "tool_only_wait",
    )

    private fun receipt() = completedTurn("receipt").copy(
        presentationClass = "task",
        inputs = listOf(input()),
        resultReasonKind = "reducer_only",
    )

    private fun input() = HolonTurnInput(
        messageId = "message",
        preview = "same text",
        actorDisplayName = null,
        presentationClass = "task",
        taskResult = result(),
    )

    private fun result() = HolonTaskResultPresentation(
        taskId = "task",
        status = "completed",
        summary = null,
        preview = "same text",
        runtimeOnly = true,
    )
}
