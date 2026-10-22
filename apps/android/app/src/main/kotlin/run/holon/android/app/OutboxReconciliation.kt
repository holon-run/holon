package run.holon.android.app

import kotlinx.coroutines.CancellationException
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonMessageBatch

/** An enqueue completion belongs to its original conversation, not the open screen. */
internal fun HolonUiState.withEnqueuedMessage(
    entry: OutboxEntity,
    clearDraft: Boolean,
    sentAttachments: List<StagedAttachment>,
): HolonUiState {
    if (session?.scopeKey != entry.scopeKey) return this
    if (selectedAgent?.id != entry.agentId) return copy(enqueueing = false)
    val sentPaths = sentAttachments.mapTo(mutableSetOf(), StagedAttachment::localPath)
    return copy(
        draft = if (clearDraft) "" else draft,
        attachments = attachments.filterNot { it.localPath in sentPaths },
        outbox = outbox.filterNot { it.requestId == entry.requestId } + entry,
        enqueueing = false,
        error = null,
    )
}

internal fun HolonUiState.withEnqueueFailure(scopeKey: String?, agentId: String, failure: Throwable): HolonUiState =
    if (failure is CancellationException || session?.scopeKey != scopeKey || selectedAgent?.id != agentId) this
    else copy(enqueueing = false, error = humanError(failure))

/** Reconcile identity, never preview text or the age of a local request. */
internal suspend fun reconcileReceivedOutbox(
    entries: List<OutboxEntity>,
    scopeKey: String,
    agentId: String,
    visibleMessageIds: Set<String> = emptySet(),
    lookup: suspend (List<String>) -> HolonMessageBatch = { HolonMessageBatch(emptyList(), emptyList()) },
    remove: suspend (OutboxEntity) -> Unit,
) {
    val received = entries.filter {
        it.scopeKey == scopeKey && it.agentId == agentId && it.state == "received" && !it.messageId.isNullOrBlank()
    }
    received.filter { it.messageId in visibleMessageIds }.forEach { remove(it) }
    val outsideWindow = received.filterNot { it.messageId in visibleMessageIds }
    for (ids in outsideWindow.mapNotNull { it.messageId }.distinct().chunked(100)) {
        val batch = try {
            lookup(ids)
        } catch (error: Exception) {
            if (error is CancellationException || error is SessionScopeChangedException || error.isAuthenticationFailure()) throw error
            // Optional cleanup must not turn a good snapshot into an offline screen.
            if (error.isTransientNetworkFailure() ||
                (error is HolonHttpException && error.statusCode in setOf(404, 405, 501))
            ) continue
            throw error
        }
        val confirmed = batch.messages
            .filter { it.agentId == agentId && it.id in ids && it.id !in batch.missingMessageIds }
            .map { it.id }.toSet()
        outsideWindow.filter { it.messageId in confirmed }.forEach { remove(it) }
    }
}
