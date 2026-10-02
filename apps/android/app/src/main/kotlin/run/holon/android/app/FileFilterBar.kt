package run.holon.android.app

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

@Composable
internal fun FileFilterBar(search: String, onSearch: (String) -> Unit, hidden: Boolean, onHidden: () -> Unit, recent: Boolean, onSort: () -> Unit) {
    var menuOpen by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Surface(Modifier.weight(1f), shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant), color = MaterialTheme.colorScheme.surface) {
            Row(Modifier.heightIn(min = 48.dp).padding(start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Default.Search, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
                BasicTextField(value = search, onValueChange = onSearch, singleLine = true,
                    textStyle = MaterialTheme.typography.bodyMedium.copy(color = MaterialTheme.colorScheme.onSurface),
                    cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                    modifier = Modifier.weight(1f).heightIn(min = 48.dp).padding(horizontal = 8.dp, vertical = 10.dp).semantics { contentDescription = ui("查找当前文件夹") },
                    decorationBox = { field -> Box(contentAlignment = Alignment.CenterStart) { if (search.isBlank()) Text(ui("查找当前文件夹"), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant); field() } })
                if (search.isNotBlank()) IconButton(onClick = { onSearch("") }) { Icon(Icons.Default.Close, contentDescription = ui("清除")) }
            }
        }
        Box {
            IconButton(onClick = { menuOpen = true }) { Icon(Icons.Default.Tune, contentDescription = ui("文件筛选与排序"), tint = if (hidden || recent) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant) }
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(text = { Text(ui("隐藏文件") + if (hidden) " ✓" else "") }, onClick = { onHidden(); menuOpen = false })
                DropdownMenuItem(text = { Text(ui(if (recent) "按名称排序" else "按修改时间排序")) }, onClick = { onSort(); menuOpen = false })
            }
        }
    }
}
