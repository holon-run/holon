@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import android.content.Intent
import android.net.Uri
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

internal fun shouldShowSavedNetworks(addingNetwork: Boolean, profiles: List<NetworkProfile>): Boolean =
    !addingNetwork && profiles.isNotEmpty()

@Composable
internal fun StartingScreen() {
    Box(
        Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
            HolonMark()
            CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
            Text(ui("正在恢复 Holon…"), color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
internal fun LoginScreen(state: HolonUiState, viewModel: ConnectionActions, addingNetwork: Boolean = false) {
    val isInsecureHttp = state.baseUrl.trim().startsWith("http://", ignoreCase = true)
    val context = LocalContext.current
    val scanner = remember(context) { GmsBarcodeScanning.getClient(context) }
    var showLanguagePicker by remember { mutableStateOf(false) }
    if (showLanguagePicker) AppLanguagePicker { showLanguagePicker = false }
    state.pendingPairing?.let { pairing ->
        AlertDialog(
            onDismissRequest = viewModel::cancelPairing,
            title = { Text(ui("连接到这台 Holon？")) },
            text = {
                Text(
                    ui("目标地址：${pairing.address}\n") +
                        if (pairing.address.startsWith("http://")) {
                            ui("HTTP 不加密，配对票据和会话可能被同一网络上的其他人截获。仅在可信局域网中继续；推荐使用 Tailscale HTTPS。")
                        } else {
                            ui("确认这是你信任的 Holon 主机。配对码只可使用一次。")
                        },
                )
            },
            confirmButton = { TextButton(onClick = viewModel::confirmPairing) { Text(ui("确认并连接")) } },
            dismissButton = { TextButton(onClick = viewModel::cancelPairing) { Text(ui("取消")) } },
        )
    }
    Column(
        Modifier.fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            if (addingNetwork) {
                IconButton(onClick = viewModel::cancelAddNetwork, enabled = !state.busy) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = ui("返回设置"))
                }
            } else {
                Spacer(Modifier.size(48.dp))
            }
            TextButton(onClick = { showLanguagePicker = true }) { Text(ui("语言")) }
        }
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 24.dp),
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            if (state.pendingShare != null) {
                Text(ui("登录后可选择 Agent 完成分享。"), color = MaterialTheme.colorScheme.primary)
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                HolonMark()
                Column {
                    Text(ui(if (addingNetwork) "添加网络" else "连接 Holon"), style = MaterialTheme.typography.headlineLarge)
                    Text(ui(if (addingNetwork) "连接另一台 Holon 主机" else "继续你正在进行的工作"), color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            Spacer(Modifier.height(4.dp))
            Text(
                ui(if (addingNetwork) "连接成功后切换到新网络，原网络会保留在此设备上。" else "需要一台已运行的 Holon 主机，以及该主机提供的访问令牌。"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (shouldShowSavedNetworks(addingNetwork, state.networkProfiles)) {
                Text(ui("已保存的网络"), style = MaterialTheme.typography.titleSmall)
                state.networkProfiles.forEach { profile ->
                    OutlinedButton(
                        onClick = { viewModel.switchNetwork(profile.networkId) },
                        enabled = !state.busy,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.Start) {
                            Text(profile.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Text(profile.baseUrl, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelSmall)
                        }
                    }
                }
            }
            OutlinedTextField(
                value = state.baseUrl,
                onValueChange = viewModel::setBaseUrl,
                label = { Text(ui("Holon 地址")) },
                supportingText = {
                    if (isInsecureHttp) {
                        Text(
                            ui("HTTP 本身不加密；请只在可信局域网或 Tailscale 等加密隧道中使用"),
                            color = MaterialTheme.colorScheme.tertiary,
                        )
                    } else {
                        Text(ui("例如 https://holon.example.com 或 http://100.64.0.1:7878"))
                    }
                },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                singleLine = true,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedButton(
                onClick = {
                    scanner.startScan()
                        .addOnSuccessListener { barcode ->
                            viewModel.applyScannedAddress(barcode.rawValue.orEmpty())
                        }
                        .addOnFailureListener {
                            viewModel.reportScanFailure()
                        }
                },
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(ui("扫描连接二维码"))
            }
            AnimatedVisibility(visible = isInsecureHttp) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Checkbox(
                        checked = state.allowInsecureHttp,
                        onCheckedChange = viewModel::setAllowInsecureHttp,
                        enabled = !state.busy,
                    )
                    Text(
                        ui("我确认此地址位于可信网络或加密隧道中"),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            OutlinedTextField(
                value = state.token,
                onValueChange = viewModel::setToken,
                label = { Text(ui("访问令牌（token）")) },
                supportingText = { Text(ui("登录后只保存可撤销的会话，不保存原始令牌")) },
                trailingIcon = {
                    TextButton(onClick = viewModel::toggleToken) {
                        Text(if (state.showToken) ui("隐藏") else ui("显示"))
                    }
                },
                visualTransformation = if (state.showToken) VisualTransformation.None else PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { viewModel.login() }),
                singleLine = true,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedButton(
                onClick = {
                    viewModel.startOidcLogin()?.let { url ->
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                    }
                },
                enabled = !state.busy && (!isInsecureHttp || state.allowInsecureHttp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(ui("使用组织浏览器登录"))
            }
            state.error?.let { ErrorBanner(it, viewModel::clearError) }
            Button(
                onClick = viewModel::login,
                enabled =
                    !state.busy &&
                        state.token.isNotBlank() &&
                        (!isInsecureHttp || state.allowInsecureHttp),
                modifier = Modifier.fillMaxWidth().height(52.dp),
                shape = RoundedCornerShape(10.dp),
            ) {
                if (state.busy) {
                    CircularProgressIndicator(Modifier.size(18.dp), color = MaterialTheme.colorScheme.onPrimary, strokeWidth = 2.dp)
                    Spacer(Modifier.width(10.dp))
                }
                Text(if (state.busy) ui(if (addingNetwork) "正在连接" else "正在登录") else ui(if (addingNetwork) "添加并切换" else "登录"))
            }
            Text(
                ui("HTTPS 默认安全；HTTP 需要确认。可使用组织浏览器登录，也可保留扫码和 token fallback。"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
