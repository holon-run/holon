@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import android.graphics.BitmapFactory
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.Image
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.produceState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import run.holon.android.sdk.HolonWorkspaceEntry

@Composable
internal fun WorkspaceBrowserScreen(
    state: HolonUiState,
    viewModel: FilesActions,
    modifier: Modifier,
    position: FileBrowserPosition,
) {
    val context = LocalContext.current
    val prepared = state.preparedArtifact
    val path = state.workspaceDirectory?.path.orEmpty()
    LaunchedEffect(path) {
        if (position.previousPath != path) {
            position.previousPath = path
            position.search = ""
            position.listState.scrollToItem(0)
        }
    }
    val saveFile = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { target ->
        val source = state.preparedArtifact ?: return@rememberLauncherForActivityResult
        target?.let { uri -> viewModel.saveArtifactToDevice(source, uri) }
    }
    if (prepared != null) {
        if (isReadableTextFile(prepared.mediaType, prepared.fileName)) {
            FileReaderScreen(
                artifact = prepared,
                title = ui("文件"),
                onBack = viewModel::returnFromMessageFile,
                backLabel = if (state.fileLinkOrigin != null || state.agentSection == AgentSection.Results || state.selectedBrief != null) ui("消息") else ui("文件列表"),
                onSave = viewModel::saveArtifactToDevice,
                onShare = { shareArtifact(context, prepared) },
            )
            return
        }
        Column(
            modifier.verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            TextButton(onClick = viewModel::returnFromMessageFile) {
                Text(if (state.fileLinkOrigin != null || state.agentSection == AgentSection.Results || state.selectedBrief != null) ui("‹ 返回消息") else ui("‹ 返回文件列表"))
            }
            Text(prepared.fileName, style = MaterialTheme.typography.headlineSmall)
            Text(prepared.mediaType, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            ArtifactPreview(prepared)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { saveFile.launch(prepared.fileName) }) { Text(ui("保存到设备")) }
                TextButton(onClick = { shareArtifact(context, prepared) }) { Text(ui("分享或打开")) }
            }
        }
        return
    }
    val visibleEntries = state.workspaceDirectory?.entries.orEmpty()
        .filter { entry ->
            (position.showHidden || !entry.name.startsWith('.')) &&
                (position.search.isBlank() || entry.name.contains(position.search, ignoreCase = true))
        }
        .sortedWith(
            if (position.sortRecent) {
                compareBy<HolonWorkspaceEntry> { it.type != "directory" }
                    .thenByDescending { it.modified ?: Long.MIN_VALUE }
                    .thenBy { it.name.lowercase() }
            } else {
                compareBy<HolonWorkspaceEntry> { it.type != "directory" }.thenBy { it.name.lowercase() }
            },
        )
    val recentArtifacts = state.briefs.values.sortedByDescending { it.createdAt }
        .flatMap { it.attachments }
        .filter { it.uri?.startsWith("workspace://") == true }
        .distinctBy { it.uri }
        .take(3)
    LazyColumn(
        state = position.listState,
        modifier = modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
    ) {
        if (recentArtifacts.isNotEmpty()) {
            item {
                Text(
                    ui("最近产物"),
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    style = MaterialTheme.typography.titleMedium,
                )
            }
            items(recentArtifacts, key = { it.uri.orEmpty() }) { attachment ->
                Box(Modifier.padding(horizontal = 16.dp)) {
                    ResultLinkRow(
                        label = attachment.name,
                        meta = ui("预览"),
                        onClick = { attachment.uri?.let { viewModel.prepareArtifact(it, attachment.name) } },
                    )
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
        if (state.workspaces.size > 1) {
            item {
                Row(
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    state.workspaces.forEach { workspace ->
                        FilterChip(
                            selected = workspace == state.selectedWorkspace,
                            onClick = { viewModel.selectWorkspace(workspace) },
                            label = {
                                Text(
                                    ui("${if (workspace.isActive) "当前 · " else ""}${workspace.label}"),
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            },
                        )
                    }
                }
            }
        }
        state.selectedWorkspace?.let { workspace ->
            item {
                WorkspaceBreadcrumbs(
                    path = state.workspaceDirectory?.path.orEmpty(),
                    rootLabel = workspace.label,
                    onOpen = viewModel::navigateWorkspaceTo,
                )
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
        item {
            FileFilterBar(position.search, { position.search = it }, position.showHidden, { position.showHidden = !position.showHidden }, position.sortRecent, { position.sortRecent = !position.sortRecent })
        }
        if (state.workspaceBusy) {
            item { Box(Modifier.fillMaxWidth().padding(20.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        items(visibleEntries, key = HolonWorkspaceEntry::name) { entry ->
            Surface(
                color = MaterialTheme.colorScheme.background,
                modifier = Modifier.fillMaxWidth().clickable { viewModel.openWorkspaceEntry(entry.name, entry.type == "directory") },
            ) {
                Row(Modifier.padding(horizontal = 16.dp, vertical = 11.dp), verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        imageVector = fileIcon(entry),
                        contentDescription = null,
                        tint = if (entry.type == "directory") MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(24.dp),
                    )
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Text(entry.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        if (entry.type != "directory") Text(
                            listOfNotNull(
                                formatBytes(entry.size),
                                entry.modified?.let(::relativeFileTime),
                            ).joinToString(" · "),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Text(if (entry.type == "directory") "›" else "", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            HorizontalDivider(modifier = Modifier.padding(start = 52.dp), color = MaterialTheme.colorScheme.outlineVariant)
        }
        if (!state.workspaceBusy && state.workspaces.isEmpty()) item { EmptyPage(ui("没有可浏览的 workspace"), ui("Agent 尚未连接可访问的工作区。")) }
        if (!state.workspaceBusy && state.workspaces.isNotEmpty() && visibleEntries.isEmpty()) {
            item {
                EmptyPage(
                    if (state.workspaceDirectory?.entries.isNullOrEmpty()) ui("这个文件夹是空的") else ui("没有匹配的文件"),
                    if (state.workspaceDirectory?.entries.isNullOrEmpty()) ui("返回上一级继续浏览。") else ui("调整搜索或显示隐藏文件。"),
                )
            }
        }
        item { Spacer(Modifier.height(20.dp)) }
    }
}

internal class FileBrowserPosition {
    val listState = LazyListState()
    var previousPath: String? = null
    var search by mutableStateOf("")
    var showHidden by mutableStateOf(false)
    var sortRecent by mutableStateOf(false)
}

@Composable
internal fun WorkspaceBreadcrumbs(path: String, rootLabel: String, onOpen: (String) -> Unit) {
    val parts = path.trim('/').split('/').filter(String::isNotBlank)
    Row(
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TextButton(onClick = { onOpen("") }) {
            Text(rootLabel, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        parts.forEachIndexed { index, part ->
            Text("/", color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(onClick = { onOpen(parts.take(index + 1).joinToString("/")) }) {
                Text(part, maxLines = 1)
            }
        }
    }
}

@Composable
internal fun ArtifactPreview(artifact: PreparedArtifact) {
    val file = remember(artifact.localPath) { File(artifact.localPath) }
    when {
        artifact.mediaType.startsWith("image/") -> {
            val bitmap by produceState<Result<android.graphics.Bitmap?>?>(null, artifact.localPath) {
                value = withContext(Dispatchers.IO) { runCatching { decodeSampledPreview(file) } }
            }
            if (bitmap?.getOrNull() != null) {
                var expanded by remember(artifact.localPath) { mutableStateOf(false) }
                val image = bitmap!!.getOrThrow()!!.asImageBitmap()
                Image(
                    bitmap = image,
                    contentDescription = ui("${artifact.fileName}，点按放大"),
                    modifier = Modifier.fillMaxWidth().height(240.dp).clickable { expanded = true },
                )
                if (expanded) {
                    var scale by remember { mutableFloatStateOf(1f) }
                    var pan by remember { mutableStateOf(Offset.Zero) }
                    Dialog(
                        onDismissRequest = { expanded = false },
                        properties = DialogProperties(usePlatformDefaultWidth = false),
                    ) {
                        Box(Modifier.fillMaxSize().background(Color.Black)) {
                            Image(
                                bitmap = image,
                                contentDescription = artifact.fileName,
                                modifier = Modifier.fillMaxSize()
                                    .pointerInput(Unit) {
                                        detectTransformGestures { _, drag, zoom, _ ->
                                            scale = (scale * zoom).coerceIn(1f, 4f)
                                            pan = if (scale == 1f) Offset.Zero else pan + drag
                                        }
                                    }
                                    .graphicsLayer(
                                        scaleX = scale,
                                        scaleY = scale,
                                        translationX = pan.x,
                                        translationY = pan.y,
                                    ),
                            )
                            IconButton(
                                onClick = { expanded = false },
                                modifier = Modifier.align(Alignment.TopEnd).padding(16.dp),
                            ) { Icon(Icons.Default.Close, contentDescription = ui("关闭图片预览"), tint = Color.White) }
                        }
                    }
                }
            } else {
                EmptyHint(if (bitmap == null) ui("正在读取图片…") else ui("图片无法预览，可保存或分享后打开"))
            }
        }
        else -> EmptyHint(ui("${artifact.mediaType} 不支持内置预览，可下载或分享后打开"))
    }
}

internal fun decodeSampledPreview(file: File): android.graphics.Bitmap? =
    BitmapFactory.Options().run {
        inJustDecodeBounds = true
        BitmapFactory.decodeFile(file.absolutePath, this)
        var sample = 1
        while (maxOf(outWidth, outHeight) / sample > 2_048) sample *= 2
        BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
    }
