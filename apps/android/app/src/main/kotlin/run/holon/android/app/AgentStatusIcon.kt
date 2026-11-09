package run.holon.android.app

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowCircleRight
import androidx.compose.material.icons.filled.Autorenew
import androidx.compose.material.icons.filled.Cancel
import androidx.compose.material.icons.filled.CheckCircleOutline
import androidx.compose.material.icons.filled.DeleteOutline
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.material.icons.filled.HelpOutline
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/** A static activity glyph also remains readable when system motion is disabled. */
@Composable
internal fun AgentStatusIcon(status: AgentStatusPresentation, modifier: Modifier = Modifier) {
    Icon(
        imageVector = when (status.kind) {
            AgentStatusKind.Stopped -> Icons.Default.Cancel
            AgentStatusKind.NeedsInput -> Icons.Default.ErrorOutline
            AgentStatusKind.Running -> Icons.Default.Autorenew
            AgentStatusKind.Runnable -> Icons.Default.ArrowCircleRight
            AgentStatusKind.Waiting -> Icons.Default.Schedule
            AgentStatusKind.Blocked -> Icons.Default.Lock
            AgentStatusKind.Ready -> Icons.Default.CheckCircleOutline
            AgentStatusKind.Unknown -> Icons.Default.HelpOutline
            AgentStatusKind.Deleting -> Icons.Default.DeleteOutline
        },
        contentDescription = status.label,
        tint = toneColor(status.kind.tone),
        modifier = modifier.size(16.dp),
    )
}
