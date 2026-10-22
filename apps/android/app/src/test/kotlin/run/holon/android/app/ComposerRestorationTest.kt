package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertSame
import kotlinx.serialization.json.buildJsonObject
import run.holon.android.sdk.HolonConversationSnapshot

class ComposerRestorationTest {
    private val savedAttachment = StagedAttachment("file", "saved.txt", "text/plain", "/saved", 1)
    private val addedAttachment = savedAttachment.copy(name = "added.txt", localPath = "/added")
    private val bundle = ConversationBundle(
        HolonConversationSnapshot(buildJsonObject {}, null, emptyList(), emptyList(), "runtime", "epoch", false, null),
        emptyList(), "saved draft", listOf(savedAttachment),
    )
    private val loading = ComposerRevision(1, 1)

    @Test fun `first load restores an untouched composer`() {
        val restored = HolonUiState().withRestoredComposer(bundle, loading, loading)
        assertEquals(bundle.draft, restored.draft)
        assertEquals(bundle.attachments, restored.attachments)
    }

    @Test fun `slow load does not replace a draft edited or intentionally cleared while loading`() {
        for (draft in listOf("new draft", "")) {
            val state = HolonUiState(draft = draft)
            val restored = state.withRestoredComposer(bundle, loading, loading.copy(draft = 3))
            assertEquals(draft, restored.draft)
            assertEquals(bundle.attachments, restored.attachments)
        }
    }

    @Test fun `slow load does not replace attachments added or removed while loading`() {
        for (attachments in listOf(listOf(addedAttachment), emptyList())) {
            val state = HolonUiState(attachments = attachments)
            val restored = state.withRestoredComposer(bundle, loading, loading.copy(attachments = 2))
            assertEquals(attachments, restored.attachments)
            assertEquals(bundle.draft, restored.draft)
        }
    }

    @Test fun `load cannot replace attachments while staging is still in flight`() {
        val state = HolonUiState(attachments = listOf(addedAttachment), stagingAttachment = true)
        assertEquals(state.attachments, state.withRestoredComposer(bundle, loading, loading).attachments)
    }

    @Test fun `same conversation refresh and cache fallback keep the existing composer`() {
        val state = HolonUiState(draft = "current", attachments = listOf(addedAttachment))
        assertSame(state, state.withRestoredComposer(bundle, loading, loading, preserveExisting = true))
    }
}
