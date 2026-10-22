package run.holon.android.app

import java.io.IOException
import java.time.Instant
import java.time.ZoneId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.buildJsonObject
import kotlin.test.*
import run.holon.android.sdk.*

class OutboxReconciliationTest {
    private val session = ActiveSession(
        "network", "https://example.test/api/", HolonCurrentUser("user", null, "session"),
        "runtime", "visibility", HolonServerInfo("", "local", true, emptySet(), null),
    )
    private val agent = AgentSummary("agent", "Agent", false, "ready", "idle", "test/model", pending = 0, currentRunId = null)
    private fun entry(id: String = "message", state: String = "received") =
        OutboxEntity("request-$id", session.scopeKey, agent.id, "original text", "[]", state, id, null, 1, 99)
    private fun message(id: String = "message", agentId: String = agent.id) = HolonMessage(
        id = id, agentId = agentId, createdAt = "2026-10-01T00:00:00Z", kind = "operator",
        origin = HolonMessageOrigin("operator", buildJsonObject {}),
        authorityClass = "operator", priority = "normal",
        body = HolonMessageBody("text", buildJsonObject {}, text = "different or truncated preview"),
        raw = buildJsonObject {},
    )
    private fun batch(vararg messages: HolonMessage) = HolonMessageBatch(messages.toList(), emptyList())

    @Test fun `window or truncated input identity removes receipt without a lookup`() = runTest {
        val removed = mutableListOf<OutboxEntity>()
        reconcileReceivedOutbox(
            listOf(entry()), session.scopeKey, agent.id, setOf("message"),
            lookup = { fail("Visible identity needs no canonical lookup") }, remove = { removed += it },
        )
        assertEquals(listOf(entry()), removed)
    }

    @Test fun `old receipt outside window reconciles by canonical identity not text`() = runTest {
        val removed = mutableListOf<OutboxEntity>()
        val lookedUp = mutableListOf<List<String>>()
        reconcileReceivedOutbox(
            listOf(entry()), session.scopeKey, agent.id,
            lookup = { lookedUp += it; batch(message()) }, remove = { removed += it },
        )
        assertEquals(listOf(listOf("message")), lookedUp)
        assertEquals(listOf(entry()), removed)
    }

    @Test fun `missing receipt and unsolicited or cross agent messages do not delete local content`() = runTest {
        val entries = listOf(entry(), entry("missing"), entry("unrelated"))
        reconcileReceivedOutbox(
            entries, session.scopeKey, agent.id,
            lookup = {
                HolonMessageBatch(
                    listOf(message(agentId = "other-agent"), message("unsolicited"), message("missing")),
                    listOf("missing"),
                )
            },
            remove = { fail("No identity was confirmed") },
        )
    }

    @Test fun `other scopes agents and non received states never enter receipt reconciliation`() = runTest {
        val entries = listOf(
            entry().copy(scopeKey = "other-scope"), entry().copy(agentId = "other-agent"),
            entry().copy(messageId = null), entry().copy(messageId = ""),
        ) + listOf("pending", "sending", "unknown", "failed").map { entry(state = it) }
        reconcileReceivedOutbox(
            entries, session.scopeKey, agent.id, setOf("message"),
            lookup = { fail("No eligible receipt") }, remove = { fail("Must keep this entry") },
        )
    }

    @Test fun `optional lookup failures retain receipts without failing a good snapshot`() = runTest {
        for (error in listOf(IOException("offline"), HolonHttpException(503, null), HolonHttpException(404, null))) {
            reconcileReceivedOutbox(
                listOf(entry()), session.scopeKey, agent.id,
                lookup = { throw error }, remove = { fail("Lookup failed") },
            )
        }
    }

    @Test fun `authentication scope changes and cancellation are not swallowed`() = runTest {
        for (error in listOf(HolonHttpException(401, null), SessionScopeChangedException(), CancellationException("stopped"))) {
            val thrown = assertFails {
                reconcileReceivedOutbox(
                    listOf(entry()), session.scopeKey, agent.id,
                    lookup = { throw error }, remove = { fail("Lookup failed") },
                )
            }
            assertSame(error, thrown)
        }
    }

    @Test fun `many old receipts are looked up in bounded distinct batches`() = runTest {
        val entries = (1..205).map { entry("message-$it") } + entry("message-1").copy(requestId = "second-request")
        val sizes = mutableListOf<Int>()
        val removed = mutableListOf<OutboxEntity>()
        reconcileReceivedOutbox(
            entries, session.scopeKey, agent.id,
            lookup = { ids -> sizes += ids.size; HolonMessageBatch(ids.map { message(it) }, emptyList()) },
            remove = { removed += it },
        )
        assertEquals(listOf(100, 100, 5), sizes)
        assertEquals(entries.toSet(), removed.toSet())
        assertEquals(entries.size, removed.size)
    }

    @Test fun `switching agent during enqueue does not append or clear the new conversation`() {
        val state = HolonUiState(
            session = session, selectedAgent = agent.copy(id = "other-agent"),
            draft = "new draft", outbox = listOf(entry("other").copy(agentId = "other-agent")),
            attachments = listOf(StagedAttachment("file", "new.txt", "text/plain", "/new", 3)),
            enqueueing = true,
        )
        assertEquals(state.copy(enqueueing = false), state.withEnqueuedMessage(entry(), true, state.attachments))
    }

    @Test fun `switching login scope during enqueue cannot mutate the new screen`() {
        val state = HolonUiState(
            session = session.copy(networkId = "new-network"), selectedAgent = agent,
            draft = "new draft", enqueueing = true,
        )
        assertSame(state, state.withEnqueuedMessage(entry(), true, emptyList()))
    }

    @Test fun `enqueue completion updates only its own request and respects edited draft`() {
        val pending = entry(state = "pending")
        val state = HolonUiState(
            session = session, selectedAgent = agent, draft = "edited draft",
            outbox = listOf(pending, entry("other")), enqueueing = true,
        )
        val updated = state.withEnqueuedMessage(pending, false, emptyList())
        assertEquals("edited draft", updated.draft)
        assertEquals(1, updated.outbox.count { it.requestId == pending.requestId })
        assertEquals(2, updated.outbox.size)
        assertFalse(updated.enqueueing)
        assertEquals("", state.withEnqueuedMessage(pending, true, emptyList()).draft)
    }

    @Test fun `late enqueue failure cannot clear another scope or agent sending state`() {
        val current = HolonUiState(session = session, selectedAgent = agent, enqueueing = true, error = "current")
        val failure = IOException("old request failed")
        assertSame(current, current.withEnqueueFailure("other-scope", agent.id, failure))
        assertSame(current, current.withEnqueueFailure(session.scopeKey, "other-agent", failure))
    }

    @Test fun `cancelled enqueue leaves the current screen untouched`() {
        val current = HolonUiState(session = session, selectedAgent = agent, enqueueing = true)
        assertSame(current, current.withEnqueueFailure(session.scopeKey, agent.id, CancellationException("stale lease")))
    }

    @Test fun `current enqueue failure releases its own sending state`() {
        val current = HolonUiState(session = session, selectedAgent = agent, enqueueing = true)
        val failure = IOException("request failed")
        val updated = current.withEnqueueFailure(session.scopeKey, agent.id, failure)
        assertFalse(updated.enqueueing)
        assertEquals(humanError(failure), updated.error)
    }

    @Test fun `local message timestamp retains original day and year across restart and retry`() {
        val createdAt = Instant.parse("2025-10-01T23:59:00Z").toEpochMilli()
        val old = entry().copy(createdAt = createdAt, updatedAt = Instant.parse("2026-10-22T03:00:00Z").toEpochMilli())
        assertEquals("2025-10-01 23:59", outboxTimestamp(old.createdAt, ZoneId.of("UTC")))
        assertEquals("2025-10-02 07:59", outboxTimestamp(old.createdAt, ZoneId.of("Asia/Shanghai")))
    }
}
