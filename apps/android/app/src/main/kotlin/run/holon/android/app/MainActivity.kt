package run.holon.android.app

import android.os.Bundle
import android.os.SystemClock
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.addCallback
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver

class MainActivity : ComponentActivity() {
    private val container by lazy { AppContainer(applicationContext) }
    private val viewModel: HolonViewModel by viewModels {
        HolonViewModel.factory(application, container)
    }
    private var lastExitRequestAt = 0L

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        lifecycle.addObserver(
            LifecycleEventObserver { _, event ->
                if (event == Lifecycle.Event.ON_RESUME) viewModel.onForeground()
                if (event == Lifecycle.Event.ON_PAUSE) viewModel.onBackground()
            },
        )
        onBackPressedDispatcher.addCallback(this) {
            if (!viewModel.handleSystemBack()) {
                val now = SystemClock.elapsedRealtime()
                if (now - lastExitRequestAt <= 2_000) {
                    finish()
                } else {
                    lastExitRequestAt = now
                    Toast.makeText(this@MainActivity, "再按一次返回桌面", Toast.LENGTH_SHORT).show()
                }
            }
        }
        setContent {
            HolonTheme {
                HolonApp(viewModel)
            }
        }
    }
}
