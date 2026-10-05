package run.holon.android.app

import kotlinx.coroutines.CancellationException
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonPromptReceipt
import run.holon.android.sdk.isTransientHttpStatus

internal fun interface OutboxStore { suspend fun put(entry: OutboxEntity) }
internal fun interface OutboxSender { fun send(entry: OutboxEntity): HolonPromptReceipt }

/** Durable send state only: no navigation, Android URI, credential, or retry ID generation. */
internal class DurableOutbox(
    private val store: OutboxStore,
    private val sender: OutboxSender,
    private val now: () -> Long = System::currentTimeMillis,
) {
    suspend fun deliver(entry: OutboxEntity): OutboxEntity {
        val sending = entry.copy(state = "sending", error = null, updatedAt = now())
        store.put(sending)
        val result = try {
            val receipt = sender.send(entry)
            sending.copy(state = "received", messageId = receipt.messageId, updatedAt = now())
        } catch (error: Throwable) {
            val failed = sending.copy(
                state = when {
                    error is HolonHttpException && !isTransientHttpStatus(error.statusCode) -> "failed"
                    error is IllegalArgumentException -> "failed"
                    else -> "unknown"
                }, error = humanError(error), updatedAt = now(),
            )
            store.put(failed)
            if (error is CancellationException || error.isAuthenticationFailure()) throw error
            return failed
        }
        store.put(result)
        return result
    }
}
