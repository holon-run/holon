package run.holon.android.app

import android.content.Intent
import android.content.pm.ShortcutManager
import android.net.Uri
import androidx.core.content.FileProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import run.holon.android.sdk.AgentSummary

@RunWith(AndroidJUnit4::class)
class AgentShareIntegrationTest {
    private val context get() = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun receivesTextAndMultipleFiles() {
        val text = incomingShare(
            context,
            Intent(Intent.ACTION_SEND).setType("text/plain")
                .putExtra(Intent.EXTRA_TEXT, "Shared text"),
        )
        assertEquals("Shared text", text?.text)

        val files = arrayListOf(Uri.parse("content://example/one"), Uri.parse("content://example/two"))
        val multiple = incomingShare(
            context,
            Intent(Intent.ACTION_SEND_MULTIPLE).setType("application/pdf")
                .putParcelableArrayListExtra(Intent.EXTRA_STREAM, files),
        )
        assertEquals(files, multiple?.files?.map(SharedFile::uri))

        val trace = TraceRecorder(context).export(TraceScope.Global)
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", trace)
        val singleFile = incomingShare(
            context,
            Intent(Intent.ACTION_SEND).setType("application/x-ndjson")
                .putExtra(Intent.EXTRA_STREAM, uri),
        )
        assertEquals(trace.name, singleFile?.files?.single()?.name)
        assertTrue((singleFile?.files?.single()?.size ?: 0L) > 0L)

        val handlers = context.packageManager.queryIntentActivities(
            Intent(Intent.ACTION_SEND).setType("application/x-ndjson"),
            0,
        )
        assertTrue(handlers.any { it.activityInfo.packageName == context.packageName })
    }

    @Test
    fun publishesScopedAgentAsSystemShareTarget() {
        val manager = context.getSystemService(ShortcutManager::class.java)
        val agent = AgentSummary(
            id = "holon-tester",
            displayName = "holon-tester",
            isDefault = false,
            registryStatus = "active",
            runtimeStatus = "idle",
            effectiveModel = "test",
            pending = 0,
            currentRunId = null,
        )
        try {
            AgentShareShortcuts.publish(context, "network-a:user-a", listOf(agent))
            val shortcutId = AgentShareShortcuts.id("network-a:user-a", agent.id)
            assertTrue(manager.dynamicShortcuts.any { it.id == shortcutId })
            assertEquals(agent, AgentShareShortcuts.target("network-a:user-a", listOf(agent), shortcutId))
            assertNotEquals(shortcutId, AgentShareShortcuts.id("network-b:user-a", agent.id))
            assertNull(AgentShareShortcuts.target("network-b:user-a", listOf(agent), shortcutId))
        } finally {
            AgentShareShortcuts.publish(context, null, emptyList())
        }
    }
}
