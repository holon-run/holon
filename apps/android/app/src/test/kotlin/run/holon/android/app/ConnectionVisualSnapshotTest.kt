package run.holon.android.app

import androidx.activity.OnBackPressedDispatcher
import androidx.activity.OnBackPressedDispatcherOwner
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.CompositionLocalProvider
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleRegistry
import app.cash.paparazzi.DeviceConfig
import app.cash.paparazzi.Paparazzi
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import run.holon.android.sdk.HolonAuthenticationMethod

class ConnectionVisualSnapshotTest {
    @get:Rule
    val paparazzi = Paparazzi(
        deviceConfig = DeviceConfig.PIXEL_5.copy(locale = "en"),
        theme = "android:style/Theme.Material.Light.NoActionBar",
    )

    @Before
    fun useEnglish() {
        UiCopy.initialize(paparazzi.context)
        UiCopy.select(paparazzi.context, "en")
    }

    @After
    fun restoreLanguage() {
        UiCopy.select(paparazzi.context, null)
    }

    @Test
    fun addressStepHasNoCredentials() = snapshot(HolonUiState(phase = AppPhase.SignedOut))

    @Test
    fun localTokenStep() = snapshot(
        HolonUiState(
            phase = AppPhase.SignedOut,
            baseUrl = "https://holon.example.com/api/",
            authenticationMethod = HolonAuthenticationMethod.LocalToken,
        ),
    )

    @Test
    fun organizationStepHasNoTokenFallback() = snapshot(
        HolonUiState(
            phase = AppPhase.SignedOut,
            baseUrl = "https://holon.example.com/api/",
            authenticationMethod = HolonAuthenticationMethod.Oidc,
        ),
    )

    private fun snapshot(state: HolonUiState) {
        paparazzi.snapshot {
            CompositionLocalProvider(LocalOnBackPressedDispatcherOwner provides backOwner) {
                MaterialTheme {
                    LoginScreen(state, noActions)
                }
            }
        }
    }

    private val backOwner = object : OnBackPressedDispatcherOwner {
        override val onBackPressedDispatcher = OnBackPressedDispatcher()
        override val lifecycle = LifecycleRegistry.createUnsafe(this).apply { currentState = Lifecycle.State.RESUMED }
    }

    private val noActions = object : ConnectionActions {
        override fun deleteNetwork(networkId: String) {}
        override fun applyScannedAddress(value: String) {}
        override fun cancelAddNetwork() {}
        override fun cancelPairing() {}
        override fun clearError() {}
        override fun confirmPairing() {}
        override fun continueLogin() {}
        override fun returnToConnectionAddress() {}
        override fun browserLoginHandled() {}
        override fun reportBrowserLoginFailure() {}
        override fun login() {}
        override fun reportScanFailure() {}
        override fun setAllowInsecureHttp(value: Boolean) {}
        override fun setBaseUrl(value: String) {}
        override fun setToken(value: String) {}
        override fun startOidcLogin(): String? = null
        override fun switchNetwork(networkId: String) {}
        override fun toggleToken() {}
    }
}
