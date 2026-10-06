package run.holon.android.app

import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutManager
import android.net.Uri
import android.provider.OpenableColumns
import androidx.core.content.IntentCompat
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import androidx.core.net.toUri
import java.security.MessageDigest
import java.util.UUID
import run.holon.android.sdk.AgentSummary

internal data class SharedFile(
    val uri: Uri,
    val name: String,
    val size: Long?,
    val mediaType: String? = null,
)

internal data class PendingAgentShare(
    val id: String = UUID.randomUUID().toString(),
    val text: String,
    val files: List<SharedFile>,
    val targetShortcutId: String? = null,
    val fromTrace: Boolean = false,
    val sourceScopeKey: String? = null,
)

// EXTRA_SHORTCUT_ID is an inlined compile-time constant; reading it is safe below API 29.
@SuppressLint("InlinedApi")
internal fun incomingShare(context: Context, intent: Intent): PendingAgentShare? {
    if (intent.action != Intent.ACTION_SEND && intent.action != Intent.ACTION_SEND_MULTIPLE) return null
    val streams = buildList {
        if (intent.action == Intent.ACTION_SEND) {
            IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)?.let(::add)
        } else {
            addAll(IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java).orEmpty())
        }
        intent.clipData?.let { clip ->
            for (index in 0 until clip.itemCount) clip.getItemAt(index).uri?.let(::add)
        }
    }.distinct()
    val subject = intent.getStringExtra(Intent.EXTRA_SUBJECT).orEmpty().trim()
    val body = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString().orEmpty().trim()
    val text = listOf(subject, body).filter(String::isNotBlank).distinct().joinToString("\n\n")
    if (text.isEmpty() && streams.isEmpty()) return null
    return PendingAgentShare(
        text = text,
        files = streams.map { uri ->
            val metadata = runCatching {
                context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)
                    ?.use { cursor ->
                        if (!cursor.moveToFirst()) null else {
                            val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                            val sizeIndex = cursor.getColumnIndex(OpenableColumns.SIZE)
                            (if (nameIndex >= 0) cursor.getString(nameIndex) else null) to
                                (if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) cursor.getLong(sizeIndex) else null)
                        }
                    }
            }.getOrNull()
            SharedFile(
                uri,
                metadata?.first?.takeIf(String::isNotBlank) ?: uri.lastPathSegment.orEmpty().ifBlank { "attachment" },
                metadata?.second,
                runCatching { context.contentResolver.getType(uri) }.getOrNull() ?: intent.type,
            )
        },
        targetShortcutId = intent.getStringExtra(Intent.EXTRA_SHORTCUT_ID)
            ?: intent.getStringExtra(AgentShareShortcuts.EXTRA_SHORTCUT_ID),
    )
}

internal object AgentShareShortcuts {
    const val EXTRA_SHORTCUT_ID = "run.holon.android.app.SHARE_SHORTCUT_ID"
    private const val CATEGORY = "run.holon.android.app.AGENT_SHARE"

    fun id(scopeKey: String, agentId: String): String {
        val digest = MessageDigest.getInstance("SHA-256")
            .digest("$scopeKey\u0000$agentId".toByteArray(Charsets.UTF_8))
        return "agent-" + digest.take(16).joinToString("") { "%02x".format(it) }
    }

    fun target(scopeKey: String, agents: List<AgentSummary>, shortcutId: String?): AgentSummary? =
        shortcutId?.let { target -> agents.firstOrNull { id(scopeKey, it.id) == target } }

    fun publish(context: Context, scopeKey: String?, agents: List<AgentSummary>) {
        if (scopeKey == null) {
            ShortcutManagerCompat.removeAllDynamicShortcuts(context)
            return
        }
        val maxCount = context.getSystemService(ShortcutManager::class.java)?.maxShortcutCountPerActivity ?: 4
        val shortcuts = agents.take(maxCount.coerceAtMost(4)).map { agent ->
            val shortcutId = id(scopeKey, agent.id)
            ShortcutInfoCompat.Builder(context, shortcutId)
                .setShortLabel(agent.displayName.take(30))
                .setLongLabel(agent.displayName)
                .setIcon(IconCompat.createWithResource(context, R.drawable.ic_holon))
                .setCategories(setOf(CATEGORY))
                .setIntent(
                    Intent(Intent.ACTION_VIEW, "holon://share/$shortcutId".toUri(), context, MainActivity::class.java)
                        .putExtra(EXTRA_SHORTCUT_ID, shortcutId),
                )
                .build()
        }
        ShortcutManagerCompat.setDynamicShortcuts(context, shortcuts)
    }
}
