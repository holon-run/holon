package run.holon.android.app

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.android.play.core.appupdate.AppUpdateManagerFactory
import com.google.android.play.core.appupdate.AppUpdateOptions
import com.google.android.play.core.install.model.AppUpdateType
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

@Composable
internal fun ChannelUpdateHost(request: ReleaseUpdateResult?, onConsumed: () -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val manager = remember(context.applicationContext) { AppUpdateManagerFactory.create(context.applicationContext) }
    val scope = rememberCoroutineScope()
    val mutex = remember { Mutex() }
    var userAction by remember { mutableStateOf<Job?>(null) }
    var checking by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    var noStore by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.StartIntentSenderForResult()) { result ->
        // Cancel is a user choice, not an error and not permission to retry automatically.
        if (result.resultCode != Activity.RESULT_OK && result.resultCode != Activity.RESULT_CANCELED) failed = true
    }
    val openStore = {
        failed = false
        noStore = !openPlayStore(context)
    }
    suspend fun beginUpdate(resumeOnly: Boolean) {
        mutex.withLock {
            try {
                // AppUpdateInfo is single-use. Always obtain fresh information before each launch.
                val info = awaitPlayUpdateInfo(manager)
                when (playUpdateRoute(info.updateAvailability(), info.isUpdateTypeAllowed(AppUpdateType.IMMEDIATE), resumeOnly)) {
                    PlayUpdateRoute.None -> Unit
                    PlayUpdateRoute.Store -> openStore()
                    PlayUpdateRoute.InApp -> {
                        if (!manager.startUpdateFlowForResult(
                                info, launcher, AppUpdateOptions.newBuilder(AppUpdateType.IMMEDIATE).build(),
                            )
                        ) failed = true
                    }
                }
            } catch (error: CancellationException) {
                if (error is kotlinx.coroutines.TimeoutCancellationException) {
                    if (!resumeOnly) failed = true
                } else {
                    throw error
                }
            } catch (_: Exception) {
                if (!resumeOnly) failed = true
            }
        }
    }
    LaunchedEffect(request) {
        if (request != null) {
            onConsumed()
            if (request !is ReleaseUpdateResult.PlayAvailable) {
                openStore()
            } else if (userAction?.isActive != true) {
                userAction = scope.launch {
                    checking = true
                    failed = false
                    noStore = false
                    try { beginUpdate(resumeOnly = false) } finally { checking = false }
                }
            }
        }
    }
    DisposableEffect(lifecycleOwner, manager) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) scope.launch { beginUpdate(resumeOnly = true) }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        if (lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) {
            scope.launch { beginUpdate(resumeOnly = true) }
        }
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    if (checking || failed || noStore) {
        AlertDialog(
            onDismissRequest = { userAction?.cancel(); checking = false; failed = false; noStore = false },
            title = { Text("Google Play") },
            text = {
                Text(ui(when {
                    checking -> "正在检查 Google Play 更新…"
                    noStore -> "无法打开 Google Play，请稍后重试。"
                    else -> "无法启动应用内更新，可前往 Google Play 查看。"
                }))
            },
            confirmButton = {
                if (!checking && !noStore) TextButton(onClick = openStore) { Text(ui("打开 Google Play")) }
            },
            dismissButton = {
                TextButton(onClick = { userAction?.cancel(); checking = false; failed = false; noStore = false }) {
                    Text(ui(if (checking) "取消" else "关闭"))
                }
            },
        )
    }
}

private fun openPlayStore(context: Context): Boolean {
    val packageName = context.packageName
    for (intent in listOf(
        Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=$packageName")).setPackage("com.android.vending"),
        Intent(Intent.ACTION_VIEW, Uri.parse("https://play.google.com/store/apps/details?id=$packageName")),
    )) {
        try {
            context.startActivity(intent)
            return true
        } catch (_: ActivityNotFoundException) {
            // A device without the Play Store can still open its HTTPS listing.
        } catch (_: SecurityException) {
            // Try the public listing rather than failing the app.
        }
    }
    return false
}
