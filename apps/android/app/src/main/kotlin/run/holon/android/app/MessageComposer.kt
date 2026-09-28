@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import android.graphics.BitmapFactory
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@Composable
internal fun MessageComposer(
    draft: String,
    attachments: List<StagedAttachment>,
    sending: Boolean,
    staging: Boolean,
    canStop: Boolean,
    stopping: Boolean,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
    onStop: () -> Unit,
    onRemove: (Int) -> Unit,
    onImage: () -> Unit,
    onFile: () -> Unit,
    onCamera: () -> Unit,
) {
    var attachmentsOpen by remember { mutableStateOf(false) }
    var expanded by remember { mutableStateOf(false) }
    var confirmStop by remember { mutableStateOf(false) }
    var editor by rememberSaveable(stateSaver = TextFieldValue.Saver) { mutableStateOf(TextFieldValue(draft, TextRange(draft.length))) }
    LaunchedEffect(draft) {
        if (draft != editor.text) editor = TextFieldValue(draft, TextRange(editor.selection.end.coerceAtMost(draft.length)))
    }
    val edit: (TextFieldValue) -> Unit = { value -> editor = value; onDraft(value.text) }
    Surface(color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 10.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            if (staging) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (attachments.isNotEmpty()) Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                attachments.forEachIndexed { index, attachment ->
                    Surface(shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant)) {
                        Row(Modifier.padding(start = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                            AttachmentThumbnail(attachment)
                            Column(Modifier.widthIn(max = 160.dp).padding(horizontal = 8.dp)) {
                                Text(attachment.name, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
                                Text(readerFileSize(attachment.size), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                            IconButton(onClick = { onRemove(index) }, enabled = !sending && !staging) { Icon(Icons.Default.Close, ui("移除 ${attachment.name}"), Modifier.size(18.dp)) }
                        }
                    }
                }
            }
            if (canStop || draft.length > 160 || '\n' in draft) Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                if (canStop) TextButton(onClick = { confirmStop = true }, enabled = !stopping) { Text(ui(if (stopping) "正在停止本轮…" else "停止本轮")) }
                Spacer(Modifier.weight(1f))
                if (draft.length > 160 || '\n' in draft) IconButton(onClick = { expanded = true }) { Icon(Icons.Default.OpenInFull, ui("展开编辑"), Modifier.size(18.dp)) }
            }
            Surface(shape = RoundedCornerShape(24.dp), border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant), color = MaterialTheme.colorScheme.surface) {
                Row(Modifier.fillMaxWidth().padding(4.dp), verticalAlignment = Alignment.Bottom) {
                    IconButton(onClick = { attachmentsOpen = true }, enabled = !sending && !staging) { Icon(Icons.Default.Add, ui("添加附件")) }
                    BasicTextField(
                        value = editor,
                        onValueChange = edit,
                        enabled = !sending,
                        textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
                        cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                        minLines = 1,
                        maxLines = 5,
                        modifier = Modifier.weight(1f).padding(vertical = 12.dp, horizontal = 4.dp),
                        decorationBox = { inner ->
                            Box {
                                if (draft.isEmpty()) Text(ui("给 Agent 发送消息…"), style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                inner()
                            }
                        },
                    )
                    FilledIconButton(onClick = onSend, enabled = !sending && !staging && (draft.isNotBlank() || attachments.isNotEmpty())) {
                        if (sending) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                        else Icon(Icons.AutoMirrored.Filled.Send, ui("发送"), Modifier.size(20.dp))
                    }
                }
            }
        }
    }
    if (attachmentsOpen) ModalBottomSheet(onDismissRequest = { attachmentsOpen = false }) {
        Text(ui("添加附件"), style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(16.dp))
        listOf(Triple(ui("从相册选择"), Icons.Default.PhotoLibrary, onImage), Triple(ui("拍照"), Icons.Default.PhotoCamera, onCamera), Triple(ui("选择文件"), Icons.Default.AttachFile, onFile)).forEach { (label, icon, action) ->
            ListItem(headlineContent = { Text(label) }, leadingContent = { Icon(icon, null) }, modifier = Modifier.clickable { attachmentsOpen = false; action() })
        }
        Spacer(Modifier.height(16.dp))
    }
    if (confirmStop) AlertDialog(onDismissRequest = { confirmStop = false }, title = { Text(ui("停止本轮？")) }, text = { Text(ui("已完成的结果会保留，当前执行将被中断。")) }, confirmButton = { TextButton(onClick = { confirmStop = false; onStop() }) { Text(ui("停止本轮")) } }, dismissButton = { TextButton(onClick = { confirmStop = false }) { Text(ui("取消")) } })
    if (expanded) Dialog(onDismissRequest = { expanded = false }, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().statusBarsPadding().navigationBarsPadding().imePadding()) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    IconButton(onClick = { expanded = false }) { Icon(Icons.Default.Close, ui("收起编辑")) }
                    Text(ui("编辑消息"), modifier = Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                    TextButton(onClick = { expanded = false }) { Text(ui("完成")) }
                }
                OutlinedTextField(value = editor, onValueChange = edit, enabled = !sending, modifier = Modifier.fillMaxSize().padding(12.dp), placeholder = { Text(ui("给 Agent 发送消息…")) })
            }
        }
    }
}

@Composable
private fun AttachmentThumbnail(attachment: StagedAttachment) {
    val bitmap by produceState<android.graphics.Bitmap?>(null, attachment.localPath) {
        if (attachment.kind == "image") value = withContext(Dispatchers.IO) {
            runCatching {
                val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                BitmapFactory.decodeFile(attachment.localPath, bounds)
                val options = BitmapFactory.Options().apply {
                    inSampleSize = 1
                    while (bounds.outWidth / inSampleSize > 192 || bounds.outHeight / inSampleSize > 192) inSampleSize *= 2
                }
                BitmapFactory.decodeFile(attachment.localPath, options)
            }.getOrNull()
        }
    }
    if (bitmap != null) Image(bitmap!!.asImageBitmap(), contentDescription = ui("图片"), modifier = Modifier.size(40.dp), contentScale = ContentScale.Crop)
    else Icon(if (attachment.kind == "image") Icons.Default.Image else Icons.Default.AttachFile, null, Modifier.size(24.dp))
}
