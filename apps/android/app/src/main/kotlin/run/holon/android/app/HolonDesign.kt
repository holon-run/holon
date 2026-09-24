package run.holon.android.app

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.Typography
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.sin

internal val HolonAccent = Color(0xFF087EA4)
internal val HolonAccentSoft = Color(0xFFE2F3F8)
internal val HolonPage = Color(0xFFFCFDFD)
internal val HolonSidebar = Color(0xFFF7F7F8)
internal val HolonSurfaceSoft = Color(0xFFF3F7FA)
internal val HolonLine = Color(0xFFE6E7E9)
internal val HolonLineStrong = Color(0xFFC3D0DA)
internal val HolonText = Color(0xFF242628)
internal val HolonMuted = Color(0xFF5C6B7A)
internal val HolonFaint = Color(0xFF8794A3)
internal val HolonSuccess = Color(0xFF16834F)
internal val HolonWarning = Color(0xFFA16207)
internal val HolonDanger = Color(0xFFC24158)

private val HolonColors =
    lightColorScheme(
        primary = HolonAccent,
        onPrimary = Color.White,
        primaryContainer = HolonAccentSoft,
        onPrimaryContainer = HolonAccent,
        secondary = HolonMuted,
        onSecondary = Color.White,
        secondaryContainer = HolonAccentSoft,
        onSecondaryContainer = HolonAccent,
        background = HolonPage,
        onBackground = HolonText,
        surface = Color.White,
        onSurface = HolonText,
        surfaceVariant = HolonSurfaceSoft,
        onSurfaceVariant = HolonMuted,
        outline = HolonLineStrong,
        outlineVariant = HolonLine,
        error = HolonDanger,
        onError = Color.White,
    )

private val HolonDarkColors =
    androidx.compose.material3.darkColorScheme(
        primary = Color(0xFF65C9EE),
        onPrimary = Color(0xFF003546),
        primaryContainer = Color(0xFF123D4A),
        onPrimaryContainer = Color(0xFFA8E2F5),
        secondary = Color(0xFFB6C7D5),
        onSecondary = Color(0xFF20333F),
        secondaryContainer = Color(0xFF123D4A),
        onSecondaryContainer = Color(0xFFA8E2F5),
        background = Color(0xFF111416),
        onBackground = Color(0xFFE4E8EA),
        surface = Color(0xFF181C1F),
        onSurface = Color(0xFFE4E8EA),
        surfaceVariant = Color(0xFF20272C),
        onSurfaceVariant = Color(0xFFB6C1C8),
        outline = Color(0xFF71808A),
        outlineVariant = Color(0xFF343C41),
        error = Color(0xFFFFB2BC),
        onError = Color(0xFF67001E),
    )

private val HolonTypography =
    Typography(
        headlineLarge =
            Typography().headlineLarge.copy(
                fontFamily = FontFamily.SansSerif,
                fontSize = 30.sp,
                lineHeight = 34.sp,
                fontWeight = FontWeight.Light,
                letterSpacing = (-0.6).sp,
            ),
        headlineSmall =
            Typography().headlineSmall.copy(
                fontFamily = FontFamily.SansSerif,
                fontSize = 20.sp,
                lineHeight = 26.sp,
                fontWeight = FontWeight.SemiBold,
                letterSpacing = (-0.2).sp,
            ),
        titleMedium =
            Typography().titleMedium.copy(
                fontSize = 15.sp,
                lineHeight = 21.sp,
                fontWeight = FontWeight.SemiBold,
            ),
        bodyLarge =
            Typography().bodyLarge.copy(
                fontSize = 15.sp,
                lineHeight = 23.sp,
            ),
        bodyMedium =
            Typography().bodyMedium.copy(
                fontSize = 14.sp,
                lineHeight = 21.sp,
            ),
        bodySmall =
            Typography().bodySmall.copy(
                fontSize = 12.sp,
                lineHeight = 18.sp,
                color = HolonMuted,
            ),
        labelMedium =
            Typography().labelMedium.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = 11.sp,
                lineHeight = 16.sp,
                fontWeight = FontWeight.Medium,
                letterSpacing = 0.2.sp,
            ),
        labelSmall =
            Typography().labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = 10.sp,
                lineHeight = 14.sp,
                letterSpacing = 0.4.sp,
            ),
    )

@Composable
internal fun HolonTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    MaterialTheme(
        colorScheme = if (darkTheme) HolonDarkColors else HolonColors,
        typography = HolonTypography,
        content = content,
    )
}

@Composable
internal fun HolonMark(modifier: Modifier = Modifier) {
    val foreground = MaterialTheme.colorScheme.onSurface
    val accent = MaterialTheme.colorScheme.primary
    Canvas(modifier = modifier.size(42.dp)) {
        fun hexagon(radius: Float): Path {
            val center = Offset(size.width / 2f, size.height / 2f)
            return Path().apply {
                repeat(6) { index ->
                    val angle = (-90f + index * 60f) * PI.toFloat() / 180f
                    val point = Offset(
                        center.x + radius * cos(angle),
                        center.y + radius * sin(angle),
                    )
                    if (index == 0) moveTo(point.x, point.y) else lineTo(point.x, point.y)
                }
                close()
            }
        }

        drawPath(
            path = hexagon(size.minDimension * 0.40f),
            color = foreground.copy(alpha = 0.78f),
            style = Stroke(width = 3.2.dp.toPx(), cap = StrokeCap.Square),
        )
        drawPath(
            path = hexagon(size.minDimension * 0.26f),
            color = accent,
            style = Stroke(width = 4.2.dp.toPx(), cap = StrokeCap.Square),
        )
    }
}

internal enum class StatusTone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

@Composable
internal fun StatusPill(
    label: String,
    tone: StatusTone,
    modifier: Modifier = Modifier,
) {
    val color =
        when (tone) {
            StatusTone.Neutral -> MaterialTheme.colorScheme.onSurfaceVariant
            StatusTone.Accent -> MaterialTheme.colorScheme.primary
            StatusTone.Success -> HolonSuccess
            StatusTone.Warning -> HolonWarning
            StatusTone.Danger -> HolonDanger
        }
    Row(
        modifier =
            modifier
                .background(color.copy(alpha = 0.09f), RoundedCornerShape(999.dp))
                .border(1.dp, color.copy(alpha = 0.22f), RoundedCornerShape(999.dp))
                .padding(horizontal = 9.dp, vertical = 5.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier
                .size(6.dp)
                .background(color, RoundedCornerShape(999.dp)),
        )
        Text(
            text = label,
            color = color,
            style = MaterialTheme.typography.labelSmall,
        )
    }
}

@Composable
internal fun HolonSection(
    title: String,
    modifier: Modifier = Modifier,
    eyebrow: String? = null,
    content: @Composable () -> Unit,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        color = MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(12.dp),
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shadowElevation = 1.dp,
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 15.dp),
            verticalArrangement = Arrangement.spacedBy(11.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                eyebrow?.let {
                    Text(
                        text = it.uppercase(),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
                Text(
                    text = title,
                    color = MaterialTheme.colorScheme.onSurface,
                    style = MaterialTheme.typography.headlineSmall,
                )
            }
            content()
        }
    }
}

@Composable
internal fun EmptyHint(text: String) {
    Text(
        text = text,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        style = MaterialTheme.typography.bodyMedium,
        modifier =
            Modifier
                .fillMaxWidth()
                .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp))
                .padding(horizontal = 12.dp, vertical = 11.dp),
    )
}
