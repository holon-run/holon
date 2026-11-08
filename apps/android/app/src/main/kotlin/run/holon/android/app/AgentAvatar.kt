package run.holon.android.app

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAvatarExpression

private val LocalAvatarRepository = staticCompositionLocalOf<AgentAvatarRepository?> { null }
private val LocalAvatarLive = staticCompositionLocalOf { false }

@Composable
internal fun AgentAvatarResources(viewModel: HolonViewModel, state: HolonUiState, content: @Composable () -> Unit) {
    val lease by viewModel.avatarSession.collectAsStateWithLifecycle()
    val activeLease = activeAvatarLease(state, lease)
    val repository = remember(activeLease) {
        activeLease?.let(viewModel::createAvatarRepository)
    }
    DisposableEffect(repository) { onDispose { repository?.close() } }
    val live = activeLease != null && state.online && state.rosterState == AgentRosterState.Synced
    LaunchedEffect(repository, live) { if (!live) repository?.cancelPending() }
    CompositionLocalProvider(
        LocalAvatarRepository provides repository,
        LocalAvatarLive provides live,
        content = content,
    )
}

/** Decoration only; the adjacent name and readable status remain the accessible identity. */
@Composable
internal fun AgentAvatar(agent: AgentSummary, modifier: Modifier = Modifier, size: Dp = 48.dp, live: Boolean = true) {
    val repository = LocalAvatarRepository.current
    val current = live && LocalAvatarLive.current
    val expression = agent.statusPresentation(current).expression
    val pixels = avatarPixels(size.value, LocalDensity.current.density)
    val request = AgentAvatarKey(agent.id, expression, pixels)
    key(repository, request, agent.incarnation, current) {
        val bitmap by produceState<ImageBitmap?>(null) {
            val bytes = repository?.image(request, allowNetwork = current, retry = true) ?: return@produceState
            val decoded = withContext(Dispatchers.Default) {
                if (!validAvatarPng(bytes, pixels)) null
                else BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.takeIf {
                    it.width == pixels && it.height == pixels
                }?.asImageBitmap()
            }
            if (repository.isActive()) value = decoded
            if (decoded == null) repository.reject(request)
            // Neutral art is safe to reuse if this lease subsequently goes offline.
            if (decoded != null && current && expression != HolonAvatarExpression.Neutral) {
                repository.image(request.copy(expression = HolonAvatarExpression.Neutral), allowNetwork = true)
            }
        }
        Box(
            modifier.size(size).clip(RoundedCornerShape(size * .28f))
                .background(MaterialTheme.colorScheme.surfaceVariant).clearAndSetSemantics {},
            contentAlignment = Alignment.Center,
        ) {
            if (bitmap != null && repository?.isActive() == true) {
                Image(bitmap = bitmap!!, contentDescription = null, modifier = Modifier.size(size))
            } else {
                val initial = remember(agent.displayName) {
                    agent.displayName.trim().codePoints().findFirst().orElse('H'.code)
                        .let { String(Character.toChars(it)).uppercase() }
                }
                Text(initial, style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}
