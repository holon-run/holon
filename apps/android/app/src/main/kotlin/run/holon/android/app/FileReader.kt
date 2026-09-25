package run.holon.android.app

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import io.noties.prism4j.Prism4j
import io.noties.prism4j.Syntax
import io.noties.prism4j.Text as PrismText
import io.noties.prism4j.Visitor
import java.io.File
import java.io.RandomAccessFile
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

private const val TEXT_PAGE_BYTES = 16_384
private const val MARKDOWN_RENDER_LIMIT_BYTES = 256L * 1024L
private const val HIGHLIGHT_LIMIT_BYTES = 1024L * 1024L

internal fun isReadableTextFile(mediaType: String, fileName: String): Boolean =
    mediaType.startsWith("text/") || mediaType in setOf(
        "application/json", "application/javascript", "application/xml", "application/x-yaml",
        "application/x-sh", "application/x-toml", "application/toml", "application/yaml",
    ) || (mediaType == "application/octet-stream" && codeLanguage(fileName) != null)

internal fun codeLanguage(fileName: String): String? =
    when (fileName.substringAfterLast('.', "").lowercase()) {
        "rs" -> "rust"
        "kt", "kts" -> "kotlin"
        "java" -> "java"
        "js", "jsx", "mjs", "cjs", "ts", "tsx" -> "javascript"
        "py" -> "python"
        "go" -> "go"
        "c", "h" -> "c"
        "cc", "cpp", "cxx", "hpp" -> "cpp"
        "swift" -> "swift"
        "css" -> "css"
        "html", "xml", "svg" -> "markup"
        "json" -> "json"
        "yaml", "yml" -> "yaml"
        "sh", "bash", "zsh" -> "bash"
        "sql" -> "sql"
        "toml", "ini" -> "ini"
        else -> null
    }

internal data class TextBytePage(val start: Long, val endExclusive: Long)

/** Indexes page boundaries without retaining decoded content in memory. */
internal class IndexedTextFile(private val file: File, private val pageBytes: Int = TEXT_PAGE_BYTES) {
    init { require(pageBytes >= 4) }

    val pages: List<TextBytePage> = buildList {
        RandomAccessFile(file, "r").use { source ->
            val length = source.length()
            var start = 0L
            while (start < length) {
                val count = minOf(pageBytes.toLong(), length - start).toInt()
                val bytes = ByteArray(count)
                source.seek(start)
                source.readFully(bytes)
                val lastNewline = bytes.indexOfLast { it == '\n'.code.toByte() }
                val cut = when {
                    start + count == length -> count
                    lastNewline >= pageBytes / 4 -> lastNewline + 1
                    else -> safeUtf8Cut(bytes)
                }
                add(TextBytePage(start, start + cut))
                start += cut
            }
        }
    }

    fun readPage(index: Int): String {
        val page = pages[index]
        val bytes = ByteArray((page.endExclusive - page.start).toInt())
        RandomAccessFile(file, "r").use { source ->
            source.seek(page.start)
            source.readFully(bytes)
        }
        return String(bytes, Charsets.UTF_8)
    }

    private fun safeUtf8Cut(bytes: ByteArray): Int {
        var lead = bytes.lastIndex
        while (lead >= 0 && bytes[lead].toInt() and 0xC0 == 0x80) lead--
        if (lead < 0) return bytes.size
        val first = bytes[lead].toInt() and 0xFF
        val expected = when {
            first and 0xF8 == 0xF0 -> 4
            first and 0xF0 == 0xE0 -> 3
            first and 0xE0 == 0xC0 -> 2
            else -> 1
        }
        return if (bytes.size - lead < expected) lead.coerceAtLeast(1) else bytes.size
    }
}

@Composable
internal fun FileReaderScreen(
    artifact: PreparedArtifact,
    title: String,
    onBack: () -> Unit,
    backLabel: String = if (title == ui("完整计划")) ui("工作详情") else ui("文件列表"),
    onSave: (PreparedArtifact, Uri) -> Unit,
    onShare: () -> Unit,
) {
    val file = remember(artifact.localPath) { File(artifact.localPath) }
    val language = remember(artifact.fileName) { codeLanguage(artifact.fileName) }
    val isMarkdown = artifact.mediaType == "text/markdown" || artifact.fileName.endsWith(".md", ignoreCase = true)
    val canRenderMarkdown = isMarkdown && file.length() <= MARKDOWN_RENDER_LIMIT_BYTES
    val canHighlight = language != null && file.length() <= HIGHLIGHT_LIMIT_BYTES
    var renderedMarkdown by remember(artifact.localPath) { mutableStateOf(canRenderMarkdown) }
    var highlighted by remember(artifact.localPath) { mutableStateOf(canHighlight) }
    var wrapLines by remember(artifact.localPath) { mutableStateOf(true) }
    val listState = rememberLazyListState()
    val index by produceState<Result<IndexedTextFile>?>(null, artifact.localPath) {
        value = withContext(Dispatchers.IO) { runCatching { IndexedTextFile(file) } }
    }
    val saveFile = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { target ->
        target?.let { onSave(artifact, it) }
    }

    LazyColumn(
        state = listState,
        modifier = Modifier.fillMaxSize(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(0.dp),
    ) {
        item {
            Column(Modifier.padding(bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                TextButton(onClick = onBack) { Text(ui("‹ 返回$backLabel")) }
                Text(title, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
                Text(artifact.fileName, style = MaterialTheme.typography.headlineSmall)
                Text("${readerFileSize(file.length())} · ${artifact.mediaType}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = { saveFile.launch(artifact.fileName) }) { Text(ui("保存")) }
                    TextButton(onClick = onShare) { Text(ui("分享或打开")) }
                }
                if (isMarkdown || language != null) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (canRenderMarkdown) {
                            FilterChip(selected = renderedMarkdown, onClick = { renderedMarkdown = !renderedMarkdown }, label = { Text(ui("排版")) })
                        }
                        if (canHighlight) {
                            FilterChip(selected = highlighted, onClick = { highlighted = !highlighted }, label = { Text(ui("代码高亮")) })
                        }
                        FilterChip(selected = wrapLines, onClick = { wrapLines = !wrapLines }, label = { Text(ui("自动换行")) })
                    }
                    if (isMarkdown && !canRenderMarkdown) {
                        Text(ui("文件较大，使用源码模式连续阅读"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    } else if (language != null && !canHighlight) {
                        Text(ui("文件较大，已关闭高亮以保持滚动流畅"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
        when {
            index == null -> item { CircularProgressIndicator(modifier = Modifier.padding(8.dp)) }
            index?.isFailure == true -> item {
                Text(ui("无法读取文件：${index?.exceptionOrNull()?.message ?: "文件已不可用"}"), color = MaterialTheme.colorScheme.error)
            }
            renderedMarkdown -> item { MarkdownFileBody(file) }
            else -> {
                val indexed = index!!.getOrThrow()
                if (indexed.pages.isEmpty()) item { Text(ui("文件为空"), color = MaterialTheme.colorScheme.onSurfaceVariant) }
                items(indexed.pages.size, key = { it }) { pageIndex ->
                    TextFilePage(indexed, pageIndex, language.takeIf { highlighted }, wrapLines)
                }
            }
        }
        if (index?.isSuccess == true) item {
            Text(ui("全文可滚动查看"), modifier = Modifier.padding(top = 12.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun MarkdownFileBody(file: File) {
    val body by produceState<Result<String>?>(null, file.absolutePath) {
        value = withContext(Dispatchers.IO) { runCatching { file.readText() } }
    }
    when {
        body == null -> CircularProgressIndicator(modifier = Modifier.padding(8.dp))
        body?.isFailure == true -> Text(ui("无法读取 Markdown：${body?.exceptionOrNull()?.message ?: "文件已不可用"}"), color = MaterialTheme.colorScheme.error)
        else -> MarkdownText(body!!.getOrThrow())
    }
}

@Composable
private fun TextFilePage(indexed: IndexedTextFile, index: Int, language: String?, wrapLines: Boolean) {
    val page by produceState<Result<String>?>(null, indexed, index) {
        value = withContext(Dispatchers.IO) { runCatching { indexed.readPage(index) } }
    }
    Surface(color = MaterialTheme.colorScheme.surfaceVariant) {
        val horizontal = if (wrapLines) Modifier else Modifier.horizontalScroll(rememberScrollState())
        SelectionContainer {
            when {
                page == null -> CircularProgressIndicator(modifier = Modifier.padding(8.dp))
                page?.isFailure == true -> Text(ui("这一段无法读取"), color = MaterialTheme.colorScheme.error)
                language != null -> HighlightedCodePage(page!!.getOrThrow(), language, horizontal, wrapLines)
                else -> Text(
                    page!!.getOrThrow(),
                    modifier = Modifier.fillMaxWidth().then(horizontal).padding(horizontal = 12.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface,
                    fontFamily = FontFamily.Monospace,
                    softWrap = wrapLines,
                )
            }
        }
    }
}

@Composable
private fun HighlightedCodePage(page: String, language: String, modifier: Modifier, wrapLines: Boolean) {
    val primary = MaterialTheme.colorScheme.primary
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val normal = MaterialTheme.colorScheme.onSurface
    val highlighted by produceState<AnnotatedString?>(null, page, language, primary, muted, normal) {
        value = withContext(Dispatchers.Default) {
            runCatching { CodeHighlighter.highlight(page, language, primary, muted, normal) }.getOrNull()
        }
    }
    Text(
        highlighted ?: AnnotatedString(page),
        modifier = Modifier.fillMaxWidth().then(modifier).padding(horizontal = 12.dp),
        style = MaterialTheme.typography.bodySmall,
        color = normal,
        fontFamily = FontFamily.Monospace,
        softWrap = wrapLines,
    )
}

internal object CodeHighlighter {
    private val prism = Prism4j()
    private val rustKeywords = Regex("\\b(?:as|async|await|const|crate|dyn|enum|extern|fn|impl|let|match|mod|move|mut|pub|ref|self|Self|static|struct|super|trait|type|unsafe|use|where)\\b")

    fun highlight(source: String, language: String, primary: Color, muted: Color, normal: Color): AnnotatedString {
        val grammarName = if (language == "rust") "clike" else language
        val builder = AnnotatedString.Builder()
        synchronized(prism) {
            val grammar = prism.grammar(grammarName) ?: return AnnotatedString(source)
            val visitor = object : Visitor() {
                private var depth = 0

                override fun visitText(text: PrismText) {
                    val literal = text.literal()
                    if (language != "rust" || depth != 0) {
                        builder.append(literal)
                        return
                    }
                    var from = 0
                    for (match in rustKeywords.findAll(literal)) {
                        builder.append(literal.substring(from, match.range.first))
                        val start = builder.length
                        builder.append(match.value)
                        builder.addStyle(SpanStyle(color = primary), start, builder.length)
                        from = match.range.last + 1
                    }
                    builder.append(literal.substring(from))
                }

                override fun visitSyntax(syntax: Syntax) {
                    val start = builder.length
                    depth++
                    visit(syntax.children())
                    depth--
                    val color = when (syntax.type()) {
                        "comment", "prolog", "doctype" -> muted
                        "keyword", "boolean", "builtin", "property", "tag", "selector" -> primary
                        "string", "char", "number", "function", "class-name", "attr-value" -> normal
                        else -> null
                    }
                    if (color != null && builder.length > start) {
                        builder.addStyle(SpanStyle(color = color), start, builder.length)
                    }
                }
            }
            visitor.visit(prism.tokenize(source, grammar))
        }
        return builder.toAnnotatedString()
    }
}

private fun readerFileSize(bytes: Long): String = when {
    bytes < 1024 -> "$bytes B"
    bytes < 1024L * 1024L -> "%.1f KB".format(bytes / 1024.0)
    else -> "%.1f MB".format(bytes / (1024.0 * 1024.0))
}
