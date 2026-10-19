package run.holon.android.app

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle

private data class BrowserTarget(val url: String, val lease: SessionLease)

/** Only classified Markdown web links enter here. Never replace the global UriHandler. */
@Composable
internal fun RuntimeBrowserHost(state: HolonUiState, viewModel: HolonViewModel, content: @Composable () -> Unit) {
    val lease by viewModel.runtimeBrowserSession.collectAsStateWithLifecycle()
    val currentLease by rememberUpdatedState(lease)
    val currentState by rememberUpdatedState(state)
    var target by remember { mutableStateOf<BrowserTarget?>(null) }
    var external by remember { mutableStateOf<String?>(null) }
    var message by remember { mutableStateOf<String?>(null) }
    val uriHandler = LocalUriHandler.current
    val contentLease = lease
    val opener: (String) -> Unit = remember(contentLease) {
        { href ->
            if (contentLease == null || !viewModel.isRuntimeBrowserLeaseCurrent(contentLease) || currentLease !== contentLease ||
                currentState.switchingNetworkId != null || currentState.session?.scopeKey != contentLease.session.scopeKey
            ) {
                message = "连接身份已改变，请在原连接中重新打开链接。"
            } else {
                val resolved = resolveRuntimeWebLink(href, contentLease.session.baseUrl)
                when {
                    resolved != null -> target = BrowserTarget(resolved, contentLease)
                    safeBrowserHttpUrl(href) != null -> external = href
                    href.startsWith("//") -> {
                        val scheme = safeBrowserHttpUrl(contentLease.session.baseUrl)?.scheme
                        val absolute = "$scheme:$href"
                        if (safeBrowserHttpUrl(absolute) != null) external = absolute else message = "无法安全打开此链接。"
                    }
                    else -> message = "无法安全打开此链接。"
                }
            }
        }
    }
    CompositionLocalProvider(LocalOpenWebLink provides opener, content = content)
    val open = target
    if (open != null && lease === open.lease && state.switchingNetworkId == null &&
        state.phase == AppPhase.Ready && state.session?.scopeKey == open.lease.session.scopeKey
    ) {
        RuntimeBrowserScreen(
            open, viewModel,
            isCurrent = { viewModel.isRuntimeBrowserLeaseCurrent(open.lease) && currentLease === open.lease && currentState.phase == AppPhase.Ready &&
                currentState.switchingNetworkId == null && currentState.session?.scopeKey == open.lease.session.scopeKey },
            onClose = { target = null },
            onExternal = { external = it },
        )
    } else if (open != null) {
        target = null // A→B→A must not resurrect a retired target.
    }
    external?.let { href ->
        AlertDialog(
            onDismissRequest = { external = null },
            title = { Text(ui("在系统浏览器打开")) },
            text = { Text(ui("不会传递 Holon 登录身份，可能需要另行登录。") + "\n" + href) },
            confirmButton = { TextButton(onClick = {
                external = null
                runCatching { uriHandler.openUri(href) }.onFailure { message = "没有可打开此链接的系统浏览器。" }
            }) { Text(ui("打开")) } },
            dismissButton = { TextButton(onClick = { external = null }) { Text(ui("取消")) } },
        )
    }
    message?.let { text ->
        AlertDialog(onDismissRequest = { message = null }, text = { Text(ui(text)) },
            confirmButton = { TextButton(onClick = { message = null }) { Text(ui("确定")) } })
    }
}

@Composable
private fun RuntimeBrowserScreen(
    target: BrowserTarget,
    viewModel: HolonViewModel,
    isCurrent: () -> Boolean,
    onClose: () -> Unit,
    onExternal: (String) -> Unit,
) {
    val context = LocalContext.current
    var revision by remember(target) { mutableIntStateOf(0) }
    var retry by remember(target) { mutableIntStateOf(0) }
    val currentCheck by rememberUpdatedState(isCurrent)
    var creationError by remember(target, retry) { mutableStateOf<String?>(null) }
    val page = remember(target, retry) {
        runCatching {
            RuntimeWebPage(
                context, target.url, target.lease.session.baseUrl,
                isCurrent = { currentCheck() },
                bindCookie = { write -> viewModel.bindRuntimeBrowserCookie(target.lease, target.url, write) },
                onExternal = onExternal,
                onAuthenticationFailure = { viewModel.runtimeBrowserAuthenticationFailed(target.lease) },
                onChanged = { revision++ },
            )
        }.getOrElse {
            creationError = when {
                !runCatching { RuntimeBrowserProfiles.isSupported() }.getOrDefault(false) ->
                    "当前 Android System WebView 不支持安全的身份隔离与清理，请升级 WebView，或在系统浏览器另行登录。"
                target.lease.session.isOffline -> "请先恢复当前连接的登录身份，再重新打开网页。"
                else -> "无法建立安全的网页会话，请重新验证登录；若仍失败，请重启应用后重试。"
            }
            null
        }
    }
    DisposableEffect(page) { onDispose { page?.close() } }
    val title = remember(revision, page) { page?.title.orEmpty() }
    val error = creationError ?: remember(revision, page) { page?.error }
    val loading = remember(revision, page) { page?.loading == true }
    fun back() { if (page?.back() != true) onClose() }
    Dialog(
        onDismissRequest = ::back,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    TextButton(onClick = ::back) { Text(ui("返回")) }
                    TextButton(onClick = onClose) { Text(ui("关闭")) }
                    TextButton(onClick = { page?.reload() }) { Text(ui("刷新")) }
                }
                Text(title.take(120), Modifier.padding(horizontal = 12.dp), maxLines = 1)
                Text(
                    "${target.lease.session.baseUrl}\n${target.lease.session.user.displayName ?: target.lease.session.user.userId}",
                    Modifier.padding(horizontal = 12.dp), style = MaterialTheme.typography.labelSmall, maxLines = 3,
                )
                Row {
                    TextButton(onClick = {
                        context.getSystemService(ClipboardManager::class.java).setPrimaryClip(
                            ClipData.newPlainText("Holon", page?.url ?: target.url),
                        )
                    }) { Text(ui("复制链接")) }
                    TextButton(onClick = { onExternal(page?.url ?: target.url) }) { Text(ui("系统打开")) }
                    TextButton(onClick = {
                        viewModel.revalidateRuntimeBrowserSession(target.lease) { valid ->
                            if (valid && currentCheck()) { page?.close(); retry++ }
                        }
                    }) { Text(ui("重新验证")) }
                }
                if (error != null) Text(ui(error), Modifier.padding(12.dp), color = MaterialTheme.colorScheme.error)
                if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (page != null) key(page) {
                    AndroidView(factory = { page.view }, modifier = Modifier.weight(1f).fillMaxWidth())
                }
                else Text(ui("页面未加载；登录身份不会交给不支持隔离的浏览器。"), Modifier.padding(12.dp))
            }
        }
    }
}
