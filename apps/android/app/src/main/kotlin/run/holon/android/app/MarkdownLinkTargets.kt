package run.holon.android.app

import java.io.ByteArrayOutputStream
import java.net.URI
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.util.Locale

internal sealed interface MarkdownWebLink {
    data class Web(val href: String) : MarkdownWebLink
    data class Mail(val href: String) : MarkdownWebLink
    data object Rejected : MarkdownWebLink
}

private val appPathShape = Regex("^/apps/[^/]+/[^/]+(?:/.*)?$")
private val appPathPrefix = Regex("^/apps/[^/]+/[^/]+")
private val encodedPathConfusion = Regex("%(?:2e|2f|5c|25)", RegexOption.IGNORE_CASE)
private val httpScheme = Regex("^https?:", RegexOption.IGNORE_CASE)

/** Null leaves the existing file/document classifier in charge; rejection must not fall through. */
internal fun classifyMarkdownWebLink(href: String): MarkdownWebLink? {
    val network = href.startsWith("//")
    val absolute = httpScheme.containsMatchIn(href)
    val mail = href.startsWith("mailto:", ignoreCase = true)
    val root = href.startsWith('/') && !network
    val path = href.substringBefore('?').substringBefore('#')
    val registered = root && (appPathShape.matches(path) || isMarkdownGuiPath(path))
    if (!network && !absolute && !mail && !registered && !(root && appPathPrefix.containsMatchIn(path))) return null
    return runCatching {
        require(href.none { it <= ' ' || it.isISOControl() || it == '\\' })
        val uri = URI(if (network) "https:$href" else href)
        require(validWebComponent(uri.rawQuery) && validWebComponent(uri.rawFragment))
        if (mail) {
            require(!uri.rawSchemeSpecificPart.isNullOrBlank())
            require(validWebComponent(uri.rawSchemeSpecificPart))
            MarkdownWebLink.Mail(href)
        } else {
            require(validWebPath(uri.rawPath.orEmpty()))
            if (root) {
                require(registered && uri.rawAuthority == null)
            } else {
                require(!href.startsWith("///") && (network || Regex("^https?://", RegexOption.IGNORE_CASE).containsMatchIn(href)))
                require(validHttpAuthority(uri))
            }
            MarkdownWebLink.Web(href)
        }
    }.getOrElse { MarkdownWebLink.Rejected }
}

/** Resolve from the origin, not an API base path. Never normalize a suspicious path into safety. */
internal fun resolveRuntimeWebLink(href: String, runtimeBaseUrl: String): String? = runCatching {
    require(classifyMarkdownWebLink(href) is MarkdownWebLink.Web)
    val base = URI(runtimeBaseUrl)
    require(validHttpAuthority(base))
    require(runtimeBaseUrl.none { it <= ' ' || it.isISOControl() || it == '\\' })
    val absolute = when {
        href.startsWith("//") -> "${base.scheme}:$href"
        href.startsWith('/') -> "${base.scheme}://${base.rawAuthority}$href"
        else -> href
    }
    val target = URI(absolute)
    require(validHttpAuthority(target))
    require(base.scheme.equals(target.scheme, ignoreCase = true))
    require(base.host.equals(target.host, ignoreCase = true))
    require(effectiveWebPort(base) == effectiveWebPort(target))
    absolute
}.getOrNull()

private fun validHttpAuthority(uri: URI): Boolean =
    (uri.scheme.equals("https", ignoreCase = true) || uri.scheme.equals("http", ignoreCase = true)) &&
        !uri.host.isNullOrEmpty() && uri.rawUserInfo == null &&
        uri.port in -1..65535 && !uri.rawAuthority.orEmpty().endsWith(':')

private fun effectiveWebPort(uri: URI): Int =
    if (uri.port != -1) uri.port else if (uri.scheme.equals("https", ignoreCase = true)) 443 else 80

// Keep in step with web-gui/app/src/app/routes.ts: unknown absolute paths are host files.
private fun isMarkdownGuiPath(path: String): Boolean {
    val route = path.trimEnd('/').ifEmpty { "/" }
    return route in setOf("/", "/files", "/search", "/skills", "/templates", "/settings") ||
        Regex("^/(?:skills|templates)/[^/]+$").matches(route) ||
        Regex("^/agents/[^/]+(?:/conversation|/skills/[^/]+)?$").matches(route)
}

private fun validWebPath(path: String): Boolean = path.split('/').all { segment ->
    val decoded = decodeWebComponent(segment)
    decoded != "." && decoded != ".." &&
        decoded.none { it == '/' || it == '\\' || it.isISOControl() } &&
        !encodedPathConfusion.containsMatchIn(decoded)
}

private fun validWebComponent(raw: String?): Boolean =
    raw == null || decodeWebComponent(raw).none { it.isISOControl() || it == '\\' }

/** Decode only for validation, with strict UTF-8; keep the original href for navigation. */
private fun decodeWebComponent(raw: String): String = buildString {
    var cursor = 0
    while (cursor < raw.length) {
        if (raw[cursor] != '%') {
            append(raw[cursor++])
        } else {
            val bytes = ByteArrayOutputStream()
            while (cursor < raw.length && raw[cursor] == '%') {
                require(cursor + 2 < raw.length)
                val high = raw[cursor + 1].digitToIntOrNull(16) ?: error("Invalid URL escape")
                val low = raw[cursor + 2].digitToIntOrNull(16) ?: error("Invalid URL escape")
                bytes.write(high * 16 + low)
                cursor += 3
            }
            append(
                StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(bytes.toByteArray())),
            )
        }
    }
}

internal data class BareMarkdownAppLink(val href: String, val next: Int)

/** Return App-shaped invalid candidates too, so they cannot fall through to host-file links. */
internal fun bareMarkdownAppLinkAt(text: String, start: Int): BareMarkdownAppLink? {
    if (!text.startsWith("/apps/", start)) return null
    if (start > 0 && !text[start - 1].isWhitespace() && text[start - 1] !in "(（[【:：，。；！？、") return null
    val end = (start until text.length).firstOrNull {
        text[it].isWhitespace() || text[it] in "<>\"'，。；！？、（）【】《》"
    } ?: text.length
    var candidate = text.substring(start, end).trimEnd('.', ',', ';', '!', '?', ':')
    while (candidate.endsWith(')') || candidate.endsWith(']')) {
        val close = candidate.last()
        val open = if (close == ')') '(' else '['
        if (candidate.count { it == close } <= candidate.count { it == open }) break
        candidate = candidate.dropLast(1)
    }
    if (!appPathShape.matches(candidate.substringBefore('?').substringBefore('#'))) return null
    return BareMarkdownAppLink(candidate, start + candidate.length)
}

internal data class MarkdownLinkReferences(val text: String, val destinations: Map<String, String>)

internal data class MarkdownLinkTarget(val label: String, val destination: String, val next: Int)

internal fun markdownLinkAt(text: String, start: Int, references: Map<String, String> = emptyMap()): MarkdownLinkTarget? {
    val image = text.startsWith("![", start)
    if (!image && text.getOrNull(start) != '[') return null
    val labelStart = start + if (image) 2 else 1
    var cursor = labelStart
    var labelDepth = 1
    while (cursor < text.length && labelDepth > 0) {
        when {
            text[cursor] == '\\' && cursor + 1 < text.length -> cursor += 2
            text[cursor] == '[' -> { labelDepth += 1; cursor += 1 }
            text[cursor] == ']' -> { labelDepth -= 1; cursor += 1 }
            else -> cursor += 1
        }
    }
    if (labelDepth != 0) return null
    val label = text.substring(labelStart, cursor - 1).replace(Regex("\\\\([\\[\\]()*_`!\\\\])"), "$1")
    if (text.getOrNull(cursor) != '(') {
        val referenceEnd = if (text.getOrNull(cursor) == '[') text.indexOf(']', cursor + 1) else -1
        val reference = if (referenceEnd >= 0) text.substring(cursor + 1, referenceEnd).ifBlank { label } else label
        val destination = references[markdownReferenceKey(reference)] ?: return null
        return MarkdownLinkTarget(label, destination, if (referenceEnd >= 0) referenceEnd + 1 else cursor)
    }
    cursor += 1
    val destinationStart = cursor
    var destinationDepth = 1
    while (cursor < text.length && destinationDepth > 0) {
        when {
            text[cursor] == '\\' && cursor + 1 < text.length -> cursor += 2
            text[cursor] == '(' -> { destinationDepth += 1; cursor += 1 }
            text[cursor] == ')' -> { destinationDepth -= 1; cursor += 1 }
            else -> cursor += 1
        }
    }
    if (destinationDepth != 0) return null
    val destination = markdownLinkDestination(text.substring(destinationStart, cursor - 1))
    return MarkdownLinkTarget(label, destination, cursor)
}

internal fun markdownReferenceKey(label: String): String =
    label.trim().replace(Regex("\\s+"), " ").lowercase(Locale.ROOT)

internal fun markdownLinkDestination(raw: String): String {
    val trimmed = raw.trim()
    val destination = if (trimmed.startsWith('<')) {
        Regex("""^<([^>]*)>(?:\s+(?:"[^"]*"|'[^']*'|\([^)]*\)))?$""")
            .matchEntire(trimmed)?.groupValues?.get(1) ?: trimmed
    } else {
        Regex("""^(.*?)\s+(?:"[^"]*"|'[^']*'|\([^)]*\))$""").matchEntire(trimmed)?.groupValues?.get(1) ?: trimmed
    }
    return destination.replace(Regex("\\\\([()\\\\])"), "$1")
}

internal fun markdownFenceAt(line: String): String? {
    val trimmed = line.trimStart()
    return when {
        trimmed.startsWith("```") -> trimmed.takeWhile { it == '`' }
        trimmed.startsWith("~~~") -> trimmed.takeWhile { it == '~' }
        else -> null
    }
}

internal fun isClosingMarkdownFence(line: String, fence: String): Boolean {
    val marker = markdownFenceAt(line) ?: return false
    return marker.first() == fence.first() && marker.length >= fence.length &&
        line.trimStart().drop(marker.length).isBlank()
}

/** Definitions are document-scoped, first-wins, and never collected from fenced code. */
internal fun extractMarkdownLinkReferences(source: String): MarkdownLinkReferences {
    val destinations = linkedMapOf<String, String>()
    var fence: String? = null
    val definition = Regex("^ {0,3}\\[([^]\\r\\n]+)]:\\s*(.+)$")
    val text = source.replace("\r\n", "\n").split('\n').joinToString("\n") { line ->
        val marker = markdownFenceAt(line)
        if (fence != null) {
            if (isClosingMarkdownFence(line, requireNotNull(fence))) fence = null
            line
        } else if (marker != null) {
            fence = marker
            line
        } else {
            val match = definition.matchEntire(line)
            if (match == null) line else {
                destinations.putIfAbsent(markdownReferenceKey(match.groupValues[1]), markdownLinkDestination(match.groupValues[2]))
                ""
            }
        }
    }
    return MarkdownLinkReferences(text, destinations)
}
