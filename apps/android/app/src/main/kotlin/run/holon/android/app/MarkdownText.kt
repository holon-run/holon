package run.holon.android.app

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.LinkInteractionListener
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp

internal sealed interface MarkdownBlock {
    data class Heading(val level: Int, val text: String) : MarkdownBlock

    data class Paragraph(val text: String) : MarkdownBlock

    data class ListItem(val marker: String, val text: String, val depth: Int = 0, val checked: Boolean? = null) : MarkdownBlock

    data class Table(val headers: List<String>, val rows: List<List<String>>) : MarkdownBlock

    data class Quote(val text: String) : MarkdownBlock

    data class Code(val language: String?, val text: String) : MarkdownBlock

    data object Divider : MarkdownBlock
}

internal val LocalOpenMessageFile = staticCompositionLocalOf<((MessageFileReference) -> Unit)?> { null }

internal fun parseMarkdown(source: String): List<MarkdownBlock> {
    val blocks = mutableListOf<MarkdownBlock>()
    val paragraph = mutableListOf<String>()
    val lines = source.replace("\r\n", "\n").split('\n')

    fun flushParagraph() {
        if (paragraph.isNotEmpty()) {
            blocks += MarkdownBlock.Paragraph(paragraph.joinToString("\n").trim())
            paragraph.clear()
        }
    }

    var index = 0
    while (index < lines.size) {
        val line = lines[index]
        val trimmed = line.trim()
        if (index + 1 < lines.size && isTableDelimiter(lines[index + 1])) {
            val headers = tableCells(line)
            if (headers.isNotEmpty()) {
                flushParagraph()
                index += 2
                val rows = mutableListOf<List<String>>()
                while (index < lines.size && lines[index].contains('|') && lines[index].isNotBlank()) {
                    rows += tableCells(lines[index])
                    index += 1
                }
                blocks += MarkdownBlock.Table(headers, rows)
                continue
            }
        }
        val fence = when {
            trimmed.startsWith("```") -> "```"
            trimmed.startsWith("~~~") -> "~~~"
            else -> null
        }
        if (fence != null) {
            flushParagraph()
            val language = trimmed.removePrefix(fence).trim().ifBlank { null }
            val code = mutableListOf<String>()
            index += 1
            while (index < lines.size && !lines[index].trim().startsWith(fence)) {
                code += lines[index]
                index += 1
            }
            blocks += MarkdownBlock.Code(language, code.joinToString("\n"))
            index += 1
            continue
        }
        if (trimmed.isEmpty()) {
            flushParagraph()
            index += 1
            continue
        }
        val heading = Regex("^(#{1,6})\\s+(.+)$").matchEntire(trimmed)
        val list = Regex("^(\\s{0,8})([-*+]|\\d+[.)])\\s+(.+)$").matchEntire(line)
        when {
            heading != null -> {
                flushParagraph()
                blocks += MarkdownBlock.Heading(heading.groupValues[1].length, heading.groupValues[2])
            }
            list != null -> {
                flushParagraph()
                val marker = list.groupValues[2]
                val content = list.groupValues[3]
                val task = Regex("^\\[([ xX])]\\s+(.+)$").matchEntire(content)
                blocks += MarkdownBlock.ListItem(
                    marker = if (marker.first().isDigit()) marker.trimEnd(')') else "•",
                    text = task?.groupValues?.get(2) ?: content,
                    depth = (list.groupValues[1].length / 2).coerceAtMost(4),
                    checked = task?.groupValues?.get(1)?.equals("x", ignoreCase = true),
                )
            }
            trimmed.startsWith(">") -> {
                flushParagraph()
                blocks += MarkdownBlock.Quote(trimmed.removePrefix(">").trimStart())
            }
            trimmed.matches(Regex("^(?:-{3,}|_{3,}|\\*{3,})$")) -> {
                flushParagraph()
                blocks += MarkdownBlock.Divider
            }
            else -> paragraph += line.trimEnd()
        }
        index += 1
    }
    flushParagraph()
    return blocks
}

private fun tableCells(line: String): List<String> =
    line.trim().trim('|').split(Regex("(?<!\\\\)\\|")).map { it.trim().replace("\\|", "|") }

private fun isTableDelimiter(line: String): Boolean {
    val cells = tableCells(line)
    return line.contains('|') && cells.isNotEmpty() && cells.all { it.matches(Regex(":?-{3,}:?")) }
}

@Composable
internal fun MarkdownText(markdown: String, modifier: Modifier = Modifier) {
    val blocks = remember(markdown) { parseMarkdown(markdown) }
    val context = LocalContext.current
    SelectionContainer { Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        blocks.forEach { block ->
            when (block) {
                is MarkdownBlock.Heading ->
                    InlineMarkdownText(
                        block.text,
                        when (block.level) {
                            1 -> MaterialTheme.typography.headlineSmall
                            2 -> MaterialTheme.typography.titleLarge
                            else -> MaterialTheme.typography.titleMedium
                        }.copy(fontWeight = FontWeight.SemiBold),
                    )
                is MarkdownBlock.Paragraph -> InlineMarkdownText(block.text, MaterialTheme.typography.bodyLarge)
                is MarkdownBlock.ListItem -> {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(start = (block.depth * 16).dp),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalAlignment = Alignment.Top,
                    ) {
                        Text(
                            block.checked?.let { if (it) "☑" else "☐" } ?: block.marker,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.width(24.dp),
                        )
                        InlineMarkdownText(block.text, MaterialTheme.typography.bodyLarge, Modifier.weight(1f))
                    }
                }
                is MarkdownBlock.Table -> {
                    Column(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState())) {
                        (listOf(block.headers) + block.rows).forEachIndexed { rowIndex, cells ->
                            Row {
                                block.headers.indices.forEach { columnIndex ->
                                    InlineMarkdownText(
                                        cells.getOrNull(columnIndex).orEmpty(),
                                        if (rowIndex == 0) MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold)
                                        else MaterialTheme.typography.bodyMedium,
                                        Modifier.width(156.dp).padding(8.dp),
                                    )
                                }
                            }
                            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                        }
                    }
                }
                is MarkdownBlock.Quote -> {
                    Row(
                        modifier = Modifier.fillMaxWidth().height(IntrinsicSize.Min),
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        Box(
                            Modifier.width(3.dp).fillMaxHeight()
                                .background(MaterialTheme.colorScheme.primary, RoundedCornerShape(2.dp)),
                        )
                        InlineMarkdownText(
                            block.text,
                            MaterialTheme.typography.bodyMedium.copy(
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                fontStyle = FontStyle.Italic,
                            ),
                            Modifier.weight(1f),
                        )
                    }
                }
                is MarkdownBlock.Code -> {
                    Column(
                        Modifier.fillMaxWidth()
                            .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp))
                            .padding(12.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                block.language?.uppercase() ?: "代码",
                                modifier = Modifier.weight(1f),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.primary,
                            )
                            TextButton(onClick = {
                                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                                clipboard.setPrimaryClip(ClipData.newPlainText("代码", block.text))
                                Toast.makeText(context, "代码已复制", Toast.LENGTH_SHORT).show()
                            }) { Text("复制代码") }
                        }
                        Text(
                            block.text,
                            modifier = Modifier.horizontalScroll(rememberScrollState()),
                            style = MaterialTheme.typography.bodyMedium,
                            fontFamily = FontFamily.Monospace,
                        )
                    }
                }
                MarkdownBlock.Divider -> HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
    } }
}

@Composable
private fun InlineMarkdownText(text: String, style: TextStyle, modifier: Modifier = Modifier) {
    val primary = MaterialTheme.colorScheme.primary
    val codeBackground = MaterialTheme.colorScheme.surfaceVariant
    val onOpenFile = LocalOpenMessageFile.current
    val annotated = remember(text, primary, codeBackground, onOpenFile) {
        inlineMarkdown(text, primary, codeBackground, onOpenFile)
    }
    Text(annotated, modifier = modifier, style = style)
}

internal data class MarkdownLinkTarget(val label: String, val destination: String, val next: Int)

internal fun markdownLinkAt(text: String, start: Int): MarkdownLinkTarget? {
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
    if (labelDepth != 0 || text.getOrNull(cursor) != '(') return null
    val label = text.substring(labelStart, cursor - 1).replace(Regex("\\\\([\\[\\]()*_`!\\\\])"), "$1")
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
    val destination = text.substring(destinationStart, cursor - 1).replace(Regex("\\\\([()\\\\])"), "$1")
    return MarkdownLinkTarget(label, destination, cursor)
}

private fun inlineMarkdown(
    text: String,
    primary: Color,
    codeBackground: Color,
    onOpenFile: ((MessageFileReference) -> Unit)?,
): AnnotatedString =
    buildAnnotatedString {
        var cursor = 0
        while (cursor < text.length) {
            when {
                text[cursor] == '\\' && cursor + 1 < text.length -> {
                    append(text[cursor + 1])
                    cursor += 2
                }
                text.startsWith("![", cursor) || text.startsWith("[", cursor) -> {
                    val image = text.startsWith("![", cursor)
                    val target = markdownLinkAt(text, cursor)
                    if (target != null) {
                        val label = target.label.ifBlank { if (image) "图片" else "链接" }
                        val webLink = target.destination.startsWith("https://") ||
                            target.destination.startsWith("http://") || target.destination.startsWith("mailto:")
                        if (webLink) {
                            withLink(
                                LinkAnnotation.Url(
                                    target.destination,
                                    TextLinkStyles(style = SpanStyle(color = primary, textDecoration = TextDecoration.Underline)),
                                ),
                            ) { append(if (image) "网页图片 · $label" else label) }
                        } else {
                            val reference = classifyMessageFileReference(target.destination)
                            val display = if (image) "图片 · $label" else label
                            if (reference != null && onOpenFile != null) {
                                withLink(
                                    LinkAnnotation.Clickable(
                                        tag = target.destination,
                                        styles = TextLinkStyles(style = SpanStyle(color = primary, textDecoration = TextDecoration.Underline)),
                                        linkInteractionListener = LinkInteractionListener { onOpenFile(reference) },
                                    ),
                                ) { append(display) }
                            } else {
                                append(display)
                            }
                        }
                        cursor = target.next
                    } else {
                        append(text[cursor])
                        cursor += 1
                    }
                }
                text.startsWith("**", cursor) -> {
                    val end = text.indexOf("**", cursor + 2)
                    if (end >= 0) {
                        withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(text.substring(cursor + 2, end)) }
                        cursor = end + 2
                    } else {
                        append("**")
                        cursor += 2
                    }
                }
                text.startsWith("~~", cursor) -> {
                    val end = text.indexOf("~~", cursor + 2)
                    if (end >= 0) {
                        withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough)) { append(text.substring(cursor + 2, end)) }
                        cursor = end + 2
                    } else {
                        append("~~")
                        cursor += 2
                    }
                }
                text[cursor] == '`' -> {
                    val end = text.indexOf('`', cursor + 1)
                    if (end >= 0) {
                        val value = text.substring(cursor + 1, end)
                        val reference = value.takeIf(::isInlineMessageFileReference)
                            ?.let { classifyMessageFileReference(it, literal = true) }
                        withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = codeBackground)) {
                            if (reference != null && onOpenFile != null) {
                                withLink(
                                    LinkAnnotation.Clickable(
                                        tag = value,
                                        styles = TextLinkStyles(style = SpanStyle(color = primary, textDecoration = TextDecoration.Underline)),
                                        linkInteractionListener = LinkInteractionListener { onOpenFile(reference) },
                                    ),
                                ) { append(value) }
                            } else {
                                append(value)
                            }
                        }
                        cursor = end + 1
                    } else {
                        append('`')
                        cursor += 1
                    }
                }
                text[cursor] == '*' || text[cursor] == '_' -> {
                    val marker = text[cursor]
                    val end = text.indexOf(marker, cursor + 1)
                    if (end > cursor + 1) {
                        withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(text.substring(cursor + 1, end)) }
                        cursor = end + 1
                    } else {
                        append(marker)
                        cursor += 1
                    }
                }
                bareMessageFileReferenceAt(text, cursor) != null -> {
                    val bare = requireNotNull(bareMessageFileReferenceAt(text, cursor))
                    if (onOpenFile != null) {
                        withLink(
                            LinkAnnotation.Clickable(
                                tag = bare.text,
                                styles = TextLinkStyles(style = SpanStyle(color = primary, textDecoration = TextDecoration.Underline)),
                                linkInteractionListener = LinkInteractionListener { onOpenFile(bare.reference) },
                            ),
                        ) { append(bare.text) }
                    } else {
                        append(bare.text)
                    }
                    cursor += bare.text.length
                }
                else -> {
                    val next = listOf(
                        text.indexOf('[', cursor),
                        text.indexOf("workspace://", cursor),
                        text.indexOf("file://", cursor),
                        text.indexOf('/', cursor),
                        text.indexOf('*', cursor),
                        text.indexOf('_', cursor),
                        text.indexOf('`', cursor),
                        text.indexOf('~', cursor),
                        text.indexOf('\\', cursor),
                    ).filter { it >= 0 }.minOrNull() ?: text.length
                    if (next == cursor) {
                        append(text[cursor])
                        cursor += 1
                    } else {
                        append(text.substring(cursor, next))
                        cursor = next
                    }
                }
            }
        }
    }
