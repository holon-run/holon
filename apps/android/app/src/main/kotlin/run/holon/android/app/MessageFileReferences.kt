package run.holon.android.app

import java.net.URI
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import run.holon.android.sdk.HolonFileReference

internal data class MessageFileReference(
    val reference: HolonFileReference,
    val fragment: String? = null,
)

/** Match Web's file-reference boundary. Never infer a root for relative paths. */
internal fun classifyMessageFileReference(raw: String, literal: Boolean = false): MessageFileReference? =
    runCatching {
        if (raw.startsWith("file://", ignoreCase = true)) {
            val uri = URI(raw)
            if (uri.rawAuthority != null && !uri.rawAuthority.equals("localhost", ignoreCase = true)) return@runCatching null
            if (uri.host != null && !uri.host.equals("localhost", ignoreCase = true)) return@runCatching null
            if (uri.userInfo != null || uri.port != -1 || uri.rawQuery != null) return@runCatching null
            val path = uri.path?.takeIf { it.startsWith('/') && '\u0000' !in it } ?: return@runCatching null
            MessageFileReference(HolonFileReference.AbsolutePath(path), uri.rawFragment?.let(::decodeFileComponent))
        } else if (raw.startsWith("workspace://")) {
            val uri = raw.substringBefore('#')
            val query = uri.substringAfter('?', "")
            if ('?' in uri && !Regex("root=[^&]+$").matches(query)) return@runCatching null
            if (uri.substringAfter("workspace://").substringBefore('/').isBlank() || '/' !in uri.substringAfter("workspace://")) {
                return@runCatching null
            }
            MessageFileReference(HolonFileReference.WorkspaceUri(raw), raw.substringAfter('#', "").takeIf { '#' in raw }?.let(::decodeFileComponent))
        } else {
            if (Regex("^[a-z][a-z0-9+.-]*:", RegexOption.IGNORE_CASE).containsMatchIn(raw)) return@runCatching null
            val path = if (literal) raw else decodeFileComponent(raw.substringBefore('#'))
            if (!path.startsWith('/') || '\u0000' in path || (!literal && '?' in raw.substringBefore('#'))) return@runCatching null
            MessageFileReference(HolonFileReference.AbsolutePath(path), if (literal) null else raw.substringAfter('#', "").takeIf { '#' in raw }?.let(::decodeFileComponent))
        }
    }.getOrNull()

private fun decodeFileComponent(value: String): String =
    URLDecoder.decode(value.replace("+", "%2B"), StandardCharsets.UTF_8.name())

internal fun isInlineMessageFileReference(value: String): Boolean =
    value.startsWith('/') || value.startsWith("workspace://") || value.startsWith("file://", ignoreCase = true)

internal data class BareMessageFileReference(val text: String, val reference: MessageFileReference)

/** Include plain paths used in briefs, while excluding URL slashes and slash-separated prose. */
internal fun bareMessageFileReferenceAt(text: String, start: Int): BareMessageFileReference? {
    if (start !in text.indices) return null
    val uri = text.startsWith("workspace://", start) || text.startsWith("file://", start, ignoreCase = true)
    val absolute = text[start] == '/' && !text.startsWith("//", start) &&
        (start == 0 || text[start - 1].isWhitespace() || text[start - 1] in "：([{=，；")
    if (!uri && !absolute) return null
    val end = (start until text.length).firstOrNull { index ->
        text[index].isWhitespace() || text[index] in "<>\"')]}，。；："
    } ?: text.length
    val candidate = text.substring(start, end).trimEnd('.', ',', ';', '!', '?', '，', '。', '；')
    if (candidate.length < 2) return null
    val reference = classifyMessageFileReference(candidate, literal = absolute) ?: return null
    return BareMessageFileReference(candidate, reference)
}
