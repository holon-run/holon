package run.holon.android.app

import java.io.IOException
import kotlinx.coroutines.test.runTest
import kotlin.test.*
import run.holon.android.sdk.HolonPromptReceipt

class DurableOutboxTest {
    @Test fun `lost response retains immutable request for recovery`() = runTest {
        val entry = OutboxEntity("stable-id", "scope-A", "holon-tester", "original", "[]", "pending", null, null, 1, 1)
        val stored = mutableListOf<OutboxEntity>()
        val sent = mutableListOf<OutboxEntity>()
        var first = true
        val outbox = DurableOutbox(OutboxStore { stored.add(it) }, OutboxSender {
            sent.add(it)
            if (first) { first = false; throw IOException("lost response") }
            HolonPromptReceipt(it.agentId, "same-message", "duplicate")
        }, now = { 42 })
        val unknown = outbox.deliver(entry)
        assertEquals("unknown", unknown.state)
        assertEquals("received", outbox.deliver(unknown).state)
        assertEquals(listOf("stable-id", "stable-id"), sent.map { it.requestId })
        assertEquals(listOf("original", "original"), sent.map { it.text })
        assertEquals(listOf("sending", "unknown", "sending", "received"), stored.map { it.state })
    }
}
