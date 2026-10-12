package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import run.holon.android.sdk.HolonAuthenticationMethod
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonProtocolException

class ConnectionAuthenticationTest {
    @Test
    fun `new connections do not invent a default server or authentication method`() {
        assertEquals("", defaultBaseUrl())
        val state = HolonUiState(phase = AppPhase.SignedOut, baseUrl = defaultBaseUrl())
        assertEquals("", state.baseUrl)
        assertNull(state.authenticationMethod)
        assertNull(state.browserLoginUrl)
        assertEquals("", state.token)
    }

    @Test
    fun `unrelated state updates preserve the discovered authentication step`() {
        val state = authenticatedStep().copy(error = "fixture-error")
        assertEquals(HolonAuthenticationMethod.Oidc, state.authenticationMethod)
        assertEquals("https://host.example/api/auth/oidc/native/start", state.browserLoginUrl)
    }

    @Test
    fun `changing address clears credentials and discovered authentication`() {
        val state = authenticatedStep().forConnectionAddress("https://other.example", allowHttp = false)
        assertEquals("https://other.example", state.baseUrl)
        assertAddressStep(state)
        assertFalse(state.allowInsecureHttp)
    }

    @Test
    fun `returning to address keeps the host and HTTP consent but clears credentials`() {
        val state = authenticatedStep().forConnectionAddress()
        assertEquals("http://host.example/api/", state.baseUrl)
        assertEquals(true, state.allowInsecureHttp)
        assertAddressStep(state)
    }

    @Test
    fun `revoking HTTP consent clears authentication and browser requests`() {
        val state = authenticatedStep().forConnectionAddress(allowHttp = false)
        assertAddressStep(state)
        assertFalse(state.allowInsecureHttp)
    }

    @Test
    fun `add network and cancel do not inherit a sign in method or pending browser request`() {
        assertAddressStep(authenticatedStep().forAddingNetwork())
        assertAddressStep(authenticatedStep().afterCancelAddingNetwork())
    }

    @Test
    fun `two step connection copy is available in English and Chinese`() {
        assertEquals("Next", UiCopy.translate("下一步", "en"))
        assertEquals("Change address", UiCopy.translate("更改地址", "en"))
        assertEquals("下一步", UiCopy.translate("下一步", "zh-CN"))
        assertEquals(
            "Checking the server's sign-in method…",
            UiCopy.translate("正在检查服务器登录方式…", "en"),
        )
    }

    @Test
    fun `discovery rejection is not described as an expired session`() {
        assertEquals(
            "服务器拒绝了登录方式查询，请检查服务器配置",
            authenticationMethodHumanError(HolonHttpException(401, null)),
        )
        assertEquals(
            "服务器登录方式不受支持，请检查 Holon 版本和服务器配置",
            authenticationMethodHumanError(HolonProtocolException("unknown mode")),
        )
        assertEquals(
            "找不到 Holon 主机，请检查地址",
            authenticationMethodHumanError(HolonProtocolException("request failed", java.net.UnknownHostException())),
        )
    }

    private fun authenticatedStep() = HolonUiState(
        phase = AppPhase.SignedOut,
        baseUrl = "http://host.example/api/",
        allowInsecureHttp = true,
        authenticationMethod = HolonAuthenticationMethod.Oidc,
        browserLoginUrl = "https://host.example/api/auth/oidc/native/start",
        token = "secret",
        showToken = true,
        statusMessage = "fixture-status",
        error = "fixture-error",
    )

    private fun assertAddressStep(state: HolonUiState) {
        assertNull(state.authenticationMethod)
        assertNull(state.browserLoginUrl)
        assertEquals("", state.token)
        assertFalse(state.showToken)
        assertNull(state.error)
        assertNull(state.statusMessage)
    }
}
