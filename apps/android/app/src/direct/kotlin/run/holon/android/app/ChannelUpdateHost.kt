package run.holon.android.app

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

private enum class DirectUpdatePhase {
    ConfirmDownload, Downloading, Ready, Permission, Installing, Finished, Failed, Legacy,
}

@Composable
internal fun ChannelUpdateHost(request: ReleaseUpdateResult?, onConsumed: () -> Unit) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()
    val installer = remember(context) { DirectApkInstaller(context.applicationContext) }
    val consume by rememberUpdatedState(onConsumed)
    var release by remember { mutableStateOf<AndroidRelease?>(null) }
    var phase by remember { mutableStateOf(DirectUpdatePhase.ConfirmDownload) }
    var message by remember { mutableStateOf("") }
    var operation by remember { mutableStateOf<Job?>(null) }
    val latestPhase by rememberUpdatedState(phase)
    val latestOperation by rememberUpdatedState(operation)

    val settings = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        // Returning from Settings is never permission to launch an installation.
        phase = if (context.packageManager.canRequestPackageInstalls()) DirectUpdatePhase.Ready else DirectUpdatePhase.Permission
    }
    val install = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        installer.clear()
        phase = DirectUpdatePhase.Finished
        message = if (result.resultCode == Activity.RESULT_OK) {
            ui("系统安装器报告安装成功。")
        } else {
            ui("安装已取消或失败。请重新下载并校验后重试；Holon 不会卸载现有应用。")
        }
    }

    LaunchedEffect(request) {
        if (request == null) return@LaunchedEffect
        consume()
        // Consuming clears request; keep this effect non-suspending and run work in the host scope.
        if (request !is ReleaseUpdateResult.Available) return@LaunchedEffect
        operation?.cancel()
        installer.clear()
        release = request.release
        message = updateIntegrityProblem(request.release.apkSize, request.release.apkSha256).orEmpty()
        phase = if (message.isEmpty()) DirectUpdatePhase.ConfirmDownload else DirectUpdatePhase.Legacy
    }
    DisposableEffect(installer, owner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP && latestOperation?.isActive == true &&
                latestPhase != DirectUpdatePhase.Installing
            ) {
                latestOperation?.cancel()
                installer.clear()
                message = ui("更新因应用离开前台而中断，请重新下载并校验。")
                phase = DirectUpdatePhase.Failed
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose {
            owner.lifecycle.removeObserver(observer)
            latestOperation?.cancel()
            installer.close()
        }
    }

    val selected = release ?: return
    fun dismiss() {
        operation?.cancel()
        installer.clear()
        release = null
    }
    fun download() {
        phase = DirectUpdatePhase.Downloading
        operation = scope.launch {
            try {
                installer.download(selected)
                phase = DirectUpdatePhase.Ready
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                installer.clear()
                message = error.message ?: ui("无法下载或校验安装包。")
                phase = DirectUpdatePhase.Failed
            }
        }
    }
    fun confirmInstall() {
        if (!context.packageManager.canRequestPackageInstalls()) {
            phase = DirectUpdatePhase.Permission
            return
        }
        phase = DirectUpdatePhase.Downloading
        operation = scope.launch {
            try {
                // Re-read the file, package, version and signatures after every permission/settings round trip.
                installer.validateForInstall(selected)
                if (!context.packageManager.canRequestPackageInstalls()) {
                    phase = DirectUpdatePhase.Permission
                } else {
                    val intent = installer.installationIntent()
                    installer.markHandedToInstaller()
                    phase = DirectUpdatePhase.Installing
                    install.launch(intent)
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                installer.clear()
                message = error.message ?: ui("无法打开系统安装器。")
                phase = DirectUpdatePhase.Failed
            }
        }
    }

    AlertDialog(
        onDismissRequest = { if (phase != DirectUpdatePhase.Installing) dismiss() },
        title = { Text("Holon ${selected.version}") },
        text = {
            Column {
                Text(when (phase) {
                    DirectUpdatePhase.ConfirmDownload ->
                        ui("下载并校验此更新？Android 会在安装前再次询问。") +
                            "\n${ui("下载大小：")}${selected.apkSize / 1024 / 1024} MiB"
                    DirectUpdatePhase.Downloading -> ui("正在下载或校验更新…")
                    DirectUpdatePhase.Ready -> ui("安装包已校验，是否继续打开 Android 系统安装器？")
                    DirectUpdatePhase.Permission ->
                        ui("Android 需要允许 Holon 安装更新。仅在同意后打开设置；返回后仍需再次确认安装。")
                    DirectUpdatePhase.Installing -> ui("请在 Android 系统安装器中完成安装。")
                    DirectUpdatePhase.Failed, DirectUpdatePhase.Finished -> message
                    DirectUpdatePhase.Legacy ->
                        "$message\n" + ui("可自行选择在浏览器中打开发布页面；Holon 不会下载未经校验的安装包。")
                })
                if (phase == DirectUpdatePhase.Downloading) LinearProgressIndicator()
            }
        },
        confirmButton = {
            when (phase) {
                DirectUpdatePhase.ConfirmDownload -> TextButton(onClick = ::download) { Text(ui("下载安装包")) }
                DirectUpdatePhase.Ready -> TextButton(onClick = ::confirmInstall) { Text(ui("安装更新")) }
                DirectUpdatePhase.Permission -> TextButton(onClick = {
                    try {
                        settings.launch(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}")))
                    } catch (error: RuntimeException) {
                        message = error.message ?: ui("Android 无法打开安装设置。")
                        phase = DirectUpdatePhase.Failed
                    }
                }) { Text(ui("打开安装设置")) }
                DirectUpdatePhase.Failed, DirectUpdatePhase.Finished ->
                    TextButton(onClick = ::download) { Text(ui("重新下载")) }
                DirectUpdatePhase.Legacy -> TextButton(onClick = {
                    try {
                        if (!isSecureUpdateUrl(selected.releaseUrl)) error("The release page requires HTTPS.")
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(selected.releaseUrl)))
                        dismiss()
                    } catch (error: RuntimeException) {
                        message = error.message ?: ui("没有浏览器可打开发布页面。")
                        phase = DirectUpdatePhase.Failed
                    }
                }) { Text(ui("在浏览器中打开发布页面")) }
                DirectUpdatePhase.Downloading, DirectUpdatePhase.Installing -> Unit
            }
        },
        dismissButton = {
            if (phase != DirectUpdatePhase.Installing) {
                TextButton(onClick = ::dismiss) { Text(ui(if (phase == DirectUpdatePhase.Downloading) "取消" else "关闭")) }
            }
        },
    )
}
