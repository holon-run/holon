package run.holon.android.app

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.net.http.SslError
import android.os.Message
import android.webkit.PermissionRequest
import android.webkit.ServiceWorkerClient
import android.webkit.SslErrorHandler
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.webkit.WebViewCompat
import java.io.ByteArrayInputStream

/** Owns one disposable identity lease. It never saves a page, profile name or credential. */
@SuppressLint("SetJavaScriptEnabled", "RequiresFeature")
internal class RuntimeWebPage(
    context: Context,
    private val initialUrl: String,
    private val baseUrl: String,
    private val isCurrent: () -> Boolean,
    private val bindCookie: ((String) -> Unit) -> Unit,
    private val onExternal: (String) -> Unit,
    private val onAuthenticationFailure: () -> Unit,
    private val onChanged: () -> Unit = {},
) {
    private val profileLease = RuntimeBrowserProfiles.create(context)
    @Volatile private var closed = false
    private val popups = mutableListOf<Pair<WebView, RuntimeBrowserProfile>>()
    var error: String? = null
        private set
    var loading = true
        private set
    var title = ""
        private set
    var url = initialUrl
        private set
    val view = WebView(context)

    init {
        try {
            WebViewCompat.setProfile(view, profileLease.profile.name)
            configure(view)
            view.webViewClient = navigationClient()
            view.webChromeClient = object : WebChromeClient() {
                override fun onReceivedTitle(view: WebView, title: String?) {
                    if (valid()) { this@RuntimeWebPage.title = title.orEmpty(); onChanged() }
                }

                override fun onPermissionRequest(request: PermissionRequest) { request.deny() }

                override fun onCreateWindow(view: WebView, isDialog: Boolean, isUserGesture: Boolean, resultMsg: Message): Boolean {
                    if (!valid() || !isUserGesture) return false
                    return createPopup(context, resultMsg)
                }
            }
            view.setDownloadListener { _, _, _, _, _ ->
                fail("此版本暂不支持网页下载，请在系统浏览器另行登录后下载。")
            }
            profileLease.profile.cookieManager.setAcceptCookie(true)
            profileLease.profile.cookieManager.setAcceptThirdPartyCookies(view, false)
            profileLease.profile.serviceWorkerController.apply {
                serviceWorkerWebSettings.allowFileAccess = false
                serviceWorkerWebSettings.allowContentAccess = false
                setServiceWorkerClient(object : ServiceWorkerClient() {
                    override fun shouldInterceptRequest(request: WebResourceRequest): WebResourceResponse? =
                        if (valid() && sameRuntimeOrigin(request.url.toString(), baseUrl)) null else blockedResponse()
                })
            }
            bindCookie { cookie ->
                if (valid()) {
                    profileLease.profile.cookieManager.setCookie(initialUrl, cookie) { accepted ->
                        if (valid()) {
                            if (accepted) view.loadUrl(initialUrl) else fail("无法绑定网页登录身份，请重新验证。")
                        }
                    }
                }
            }
        } catch (error: Throwable) {
            close()
            throw error
        }
    }

    private fun valid(): Boolean = !closed && isCurrent()

    private fun configure(webView: WebView) {
        webView.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            allowFileAccess = false
            allowContentAccess = false
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            javaScriptCanOpenWindowsAutomatically = false
            setSupportMultipleWindows(true)
            mediaPlaybackRequiresUserGesture = true
            setGeolocationEnabled(false)
            cacheMode = WebSettings.LOAD_NO_CACHE
        }
    }

    private fun navigationClient() = object : WebViewClient() {
        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
            if (!valid()) return true
            if (!request.isForMainFrame) return false
            return route(request.url.toString())
        }

        override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? =
            // Defense in depth, not a replacement for the runtime's CSP or browser origin policy.
            if (!valid() || !sameRuntimeOrigin(request.url.toString(), baseUrl)) blockedResponse() else null

        override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) {
            if (!valid() || !sameRuntimeOrigin(url, baseUrl)) {
                view.stopLoading()
                return
            }
            this@RuntimeWebPage.url = url
            error = null
            loading = true
            onChanged()
        }

        override fun onPageFinished(view: WebView, url: String) {
            if (valid()) { loading = false; onChanged() }
        }

        override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
            if (valid() && request.isForMainFrame) fail("页面加载失败，请检查连接后重试。")
        }

        override fun onReceivedHttpError(view: WebView, request: WebResourceRequest, response: WebResourceResponse) {
            if (!valid() || !request.isForMainFrame) return
            when (response.statusCode) {
                401 -> {
                    fail("登录已失效，请重新登录。")
                    onAuthenticationFailure()
                }
                403 -> fail("没有权限访问此页面（403）。")
                404 -> fail("页面不存在（404）。")
                else -> fail("页面返回错误（${response.statusCode}），可手动重试。")
            }
        }

        override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) {
            handler.cancel()
            if (valid()) fail("TLS 证书验证失败，已停止加载。")
        }

        override fun onFormResubmission(view: WebView, dontResend: Message, resend: Message) {
            dontResend.sendToTarget()
            if (valid()) fail("为避免重复提交，已阻止表单自动重发。")
        }
    }

    private fun route(target: String): Boolean = when (runtimeBrowserNavigation(target, baseUrl)) {
        RuntimeBrowserNavigation.Internal -> false
        RuntimeBrowserNavigation.External -> { onExternal(target); true }
        RuntimeBrowserNavigation.Blocked -> { fail("已阻止不安全的网页导航。"); true }
    }

    private fun createPopup(context: Context, result: Message): Boolean {
        val popupProfile = RuntimeBrowserProfiles.create(context)
        val popup = WebView(context)
        popups += popup to popupProfile
        try {
            // No session, script or network in a window used only to capture its destination.
            WebViewCompat.setProfile(popup, popupProfile.profile.name)
            popup.settings.apply {
                javaScriptEnabled = false
                blockNetworkLoads = true
                allowFileAccess = false
                allowContentAccess = false
            }
            popup.webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    val target = request.url.toString()
                    if (valid() && !route(target)) this@RuntimeWebPage.view.loadUrl(target)
                    view.post { discardPopup(view, popupProfile) }
                    return true
                }
                override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest) = blockedResponse()
            }
            val transport = result.obj as WebView.WebViewTransport
            transport.webView = popup
            result.sendToTarget()
            // Do not retain a blank window indefinitely if the page never supplies a destination.
            view.postDelayed({ discardPopup(popup, popupProfile) }, 5_000)
            return true
        } catch (_: Exception) {
            discardPopup(popup, popupProfile)
            fail("无法安全打开新窗口。")
            return false
        }
    }

    private fun discardPopup(popup: WebView, profile: RuntimeBrowserProfile) {
        if (popups.remove(popup to profile)) {
            popup.stopLoading()
            popup.destroy()
            profile.retire()
        }
    }

    private fun fail(message: String) { error = message; loading = false; onChanged() }

    fun back(): Boolean {
        if (!valid() || !view.canGoBack()) return false
        view.goBack()
        return true
    }

    fun reload() {
        if (valid()) {
            // Never reload a POST/history entry: retry is an explicit fresh GET of the current URL.
            error = null
            view.loadUrl(url)
        }
    }

    fun close() {
        if (closed) return
        closed = true
        popups.toList().forEach { (popup, profile) -> discardPopup(popup, profile) }
        view.stopLoading()
        view.webChromeClient = null
        view.webViewClient = WebViewClient()
        view.removeAllViews()
        view.destroy()
        profileLease.profile.serviceWorkerController.setServiceWorkerClient(object : ServiceWorkerClient() {
            override fun shouldInterceptRequest(request: WebResourceRequest) = WebResourceResponse(
                "text/plain", "UTF-8", 403, "Closed", emptyMap(), ByteArrayInputStream(byteArrayOf()),
            )
        })
        profileLease.retire()
    }

    private fun blockedResponse() = WebResourceResponse(
        "text/plain", "UTF-8", 403, "Blocked", mapOf("Cache-Control" to "no-store"), ByteArrayInputStream(byteArrayOf()),
    )
}
