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
import androidx.compose.runtime.CompositionLocalProvider
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
internal val LocalOpenWebLink = staticCompositionLocalOf<((String) -> Unit)?> { null }
private val LocalMarkdownLinkReferences = staticCompositionLocalOf<Map<String, String>> { emptyMap() }

internal fun parseMarkdown(source: String): List<MarkdownBlock> =
    parseMarkdownBlocks(extractMarkdownLinkReferences(source).text)

private fun parseMarkdownBlocks(source: String): List<MarkdownBlock> {
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
        val fence = markdownFenceAt(line)
        if (fence != null) {
            flushParagraph()
            val language = trimmed.removePrefix(fence).trim().ifBlank { null }
            val code = mutableListOf<String>()
            index += 1
            while (index < lines.size && !isClosingMarkdownFence(lines[index], fence)) {
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
    val document = remember(markdown) { extractMarkdownLinkReferences(markdown) }
    val blocks = remember(document.text) { parseMarkdownBlocks(document.text) }
    val context = LocalContext.current
    CompositionLocalProvider(LocalMarkdownLinkReferences provides document.destinations) {
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
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
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
                                block.language?.uppercase() ?: ui("代码"),
                                modifier = Modifier.weight(1f),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.primary,
                            )
                            TextButton(onClick = {
                                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                                clipboard.setPrimaryClip(ClipData.newPlainText(ui("代码"), block.text))
                                Toast.makeText(context, ui("代码已复制"), Toast.LENGTH_SHORT).show()
                            }) { Text(ui("复制代码")) }
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
}

@Composable
private fun InlineMarkdownText(text: String, style: TextStyle, modifier: Modifier = Modifier) {
    val primary = MaterialTheme.colorScheme.primary
    val codeBackground = MaterialTheme.colorScheme.surfaceVariant
    val onOpenFile = LocalOpenMessageFile.current
    val onOpenWebLink = LocalOpenWebLink.current
    val references = LocalMarkdownLinkReferences.current
    val annotated = remember(text, primary, codeBackground, onOpenFile, onOpenWebLink, references) {
        inlineMarkdown(text, primary, codeBackground, onOpenFile, onOpenWebLink, references)
    }
    Text(annotated, modifier = modifier, style = style)
}

internal fun inlineMarkdown(
    text: String,
    primary: Color,
    codeBackground: Color,
    onOpenFile: ((MessageFileReference) -> Unit)?,
    onOpenWebLink: ((String) -> Unit)? = null,
    references: Map<String, String> = emptyMap(),
): AnnotatedString =
    buildAnnotatedString {
        fun appendTarget(label: String, destination: String, image: Boolean = false) {
            val web = classifyMarkdownWebLink(destination)
            val styles = TextLinkStyles(style = SpanStyle(color = primary, textDecoration = TextDecoration.Underline))
            val display = if (image) ui(if (web is MarkdownWebLink.Web || web is MarkdownWebLink.Mail) "网页图片 · $label" else "图片 · $label") else label
            when {
                web is MarkdownWebLink.Web && onOpenWebLink != null -> {
                    withLink(
                        LinkAnnotation.Clickable(
                            tag = web.href,
                            styles = styles,
                            linkInteractionListener = LinkInteractionListener { onOpenWebLink(web.href) },
                        ),
                    ) { append(display) }
                }
                web is MarkdownWebLink.Mail ||
                    (web is MarkdownWebLink.Web && destination.startsWith("http", ignoreCase = true)) -> {
                    withLink(LinkAnnotation.Url(destination, styles)) { append(display) }
                }
                web != null -> append(display)
                else -> {
                    val reference = classifyMessageFileReference(destination)
                    if (reference != null && onOpenFile != null) {
                        withLink(
                            LinkAnnotation.Clickable(
                                tag = destination,
                                styles = styles,
                                linkInteractionListener = LinkInteractionListener { onOpenFile(reference) },
                            ),
                        ) { append(display) }
                    } else {
                        append(display)
                    }
                }
            }
        }
        var cursor = 0
        while (cursor < text.length) {
            when {
                text[cursor] == '\\' && cursor + 1 < text.length -> {
                    append(text[cursor + 1])
                    cursor += 2
                }
                text.startsWith("![", cursor) || text.startsWith("[", cursor) -> {
                    val image = text.startsWith("![", cursor)
                    val target = markdownLinkAt(text, cursor, references)
                    if (target != null) {
                        val label = target.label.ifBlank { if (image) ui("图片") else ui("链接") }
                        appendTarget(label, target.destination, image)
                        cursor = target.next
                    } else {
                        append(text[cursor])
                        cursor += 1
                    }
                }
                text.startsWith("**", cursor) -> {
                    val end = text.indexOf("**", cursor + 2)
                    if (end >= 0) {
                        withStyle(SpanStyle(fontWeight = FontWeight.Bold)) {
                            append(inlineMarkdown(text.substring(cursor + 2, end), primary, codeBackground, onOpenFile, onOpenWebLink, references))
                        }
                        cursor = end + 2
                    } else {
                        append("**")
                        cursor += 2
                    }
                }
                text.startsWith("~~", cursor) -> {
                    val end = text.indexOf("~~", cursor + 2)
                    if (end >= 0) {
                        withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough)) {
                            append(inlineMarkdown(text.substring(cursor + 2, end), primary, codeBackground, onOpenFile, onOpenWebLink, references))
                        }
                        cursor = end + 2
                    } else {
                        append("~~")
                        cursor += 2
                    }
                }
                text[cursor] == '`' -> {
                    val delimiter = text.substring(cursor).takeWhile { it == '`' }
                    var end = text.indexOf(delimiter, cursor + delimiter.length)
                    while (end >= 0 && (text.getOrNull(end - 1) == '`' || text.getOrNull(end + delimiter.length) == '`')) {
                        end = text.indexOf(delimiter, end + delimiter.length)
                    }
                    if (end >= 0) {
                        val value = text.substring(cursor + delimiter.length, end)
                        val reference = value.takeIf { classifyMarkdownWebLink(it) == null && isInlineMessageFileReference(it) }
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
                        cursor = end + delimiter.length
                    } else {
                        append(delimiter)
                        cursor += delimiter.length
                    }
                }
                text[cursor] == '*' || text[cursor] == '_' -> {
                    val marker = text[cursor]
                    val end = text.indexOf(marker, cursor + 1)
                    if (end > cursor + 1) {
                        withStyle(SpanStyle(fontStyle = FontStyle.Italic)) {
                            append(inlineMarkdown(text.substring(cursor + 1, end), primary, codeBackground, onOpenFile, onOpenWebLink, references))
                        }
                        cursor = end + 1
                    } else {
                        append(marker)
                        cursor += 1
                    }
                }
                bareMarkdownAppLinkAt(text, cursor) != null -> {
                    val bare = requireNotNull(bareMarkdownAppLinkAt(text, cursor))
                    appendTarget(bare.href, bare.href)
                    cursor = bare.next
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
