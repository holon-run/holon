package run.holon.android.app

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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
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

    data class ListItem(val marker: String, val text: String) : MarkdownBlock

    data class Quote(val text: String) : MarkdownBlock

    data class Code(val language: String?, val text: String) : MarkdownBlock

    data object Divider : MarkdownBlock
}

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
        val unordered = Regex("^[-*+]\\s+(.+)$").matchEntire(trimmed)
        val ordered = Regex("^(\\d+)[.)]\\s+(.+)$").matchEntire(trimmed)
        when {
            heading != null -> {
                flushParagraph()
                blocks += MarkdownBlock.Heading(heading.groupValues[1].length, heading.groupValues[2])
            }
            unordered != null -> {
                flushParagraph()
                blocks += MarkdownBlock.ListItem("•", unordered.groupValues[1])
            }
            ordered != null -> {
                flushParagraph()
                blocks += MarkdownBlock.ListItem("${ordered.groupValues[1]}.", ordered.groupValues[2])
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

@Composable
internal fun MarkdownText(markdown: String, modifier: Modifier = Modifier) {
    val blocks = remember(markdown) { parseMarkdown(markdown) }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
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
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalAlignment = Alignment.Top,
                    ) {
                        Text(block.marker, color = MaterialTheme.colorScheme.primary, modifier = Modifier.width(24.dp))
                        InlineMarkdownText(block.text, MaterialTheme.typography.bodyLarge, Modifier.weight(1f))
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
                        block.language?.let {
                            Text(it.uppercase(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
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
    }
}

@Composable
private fun InlineMarkdownText(text: String, style: TextStyle, modifier: Modifier = Modifier) {
    val primary = MaterialTheme.colorScheme.primary
    val codeBackground = MaterialTheme.colorScheme.surfaceVariant
    val annotated = remember(text, primary, codeBackground) { inlineMarkdown(text, primary, codeBackground) }
    Text(annotated, modifier = modifier, style = style)
}

private fun inlineMarkdown(text: String, primary: Color, codeBackground: Color): AnnotatedString =
    buildAnnotatedString {
        var cursor = 0
        while (cursor < text.length) {
            when {
                text.startsWith("![", cursor) || text.startsWith("[", cursor) -> {
                    val image = text.startsWith("![", cursor)
                    val labelStart = cursor + if (image) 2 else 1
                    val labelEnd = text.indexOf(']', labelStart)
                    val destinationStart = if (labelEnd >= 0 && text.getOrNull(labelEnd + 1) == '(') labelEnd + 2 else -1
                    val destinationEnd = if (destinationStart >= 0) text.indexOf(')', destinationStart) else -1
                    if (labelEnd >= 0 && destinationStart >= 0 && destinationEnd >= 0) {
                        val label = text.substring(labelStart, labelEnd).ifBlank { "链接" }
                        val destination = text.substring(destinationStart, destinationEnd)
                        val visibleLabel = if (image) "图片 · $label" else label
                        if (destination.startsWith("https://") || destination.startsWith("http://") || destination.startsWith("mailto:")) {
                            withLink(
                                LinkAnnotation.Url(
                                    destination,
                                    TextLinkStyles(style = SpanStyle(color = primary, textDecoration = TextDecoration.Underline)),
                                ),
                            ) { append(visibleLabel) }
                        } else {
                            withStyle(SpanStyle(color = primary, fontWeight = FontWeight.Medium)) { append(visibleLabel) }
                        }
                        cursor = destinationEnd + 1
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
                        withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = codeBackground)) {
                            append(text.substring(cursor + 1, end))
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
                else -> {
                    val next = listOf(
                        text.indexOf('[', cursor),
                        text.indexOf('*', cursor),
                        text.indexOf('_', cursor),
                        text.indexOf('`', cursor),
                        text.indexOf('~', cursor),
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
