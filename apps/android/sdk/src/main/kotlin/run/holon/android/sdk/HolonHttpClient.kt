package run.holon.android.sdk

import java.io.IOException
import kotlinx.serialization.KSerializer
import kotlinx.serialization.builtins.ListSerializer
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import run.holon.client.wire.generated.models.AgentListEntry
import run.holon.client.wire.generated.models.ErrorResponse
import run.holon.client.wire.generated.models.HandshakeResponse

public fun interface BearerTokenProvider {
    public fun token(): String?
}

public class HolonHttpException(
    public val statusCode: Int,
    public val apiError: HolonApiError?,
) : IOException(
    apiError?.let { "${it.code}: ${it.message}" }
        ?: "Holon request failed with HTTP $statusCode",
)

public class HolonProtocolException(
    message: String,
    cause: Throwable? = null,
) : IOException(message, cause)

/**
 * Blocking read-only client for the first Android SDK transport slice.
 *
 * [baseUrl] is the Holon API base URL, including `/api` for a directly connected daemon.
 *
 * Callers must execute these methods away from the Android main thread.
 */
public class HolonHttpClient internal constructor(
    baseUrl: String,
    private val bearerTokenProvider: BearerTokenProvider,
    private val httpClient: OkHttpClient,
) {
    public constructor(
        baseUrl: String,
        bearerTokenProvider: BearerTokenProvider = BearerTokenProvider { null },
    ) : this(
        baseUrl = baseUrl,
        bearerTokenProvider = bearerTokenProvider,
        httpClient = defaultHttpClient(),
    )

    private val baseUrl: HttpUrl = normalizeBaseUrl(baseUrl)

    public fun handshake(
        requiredCapabilities: Set<String> = emptySet(),
    ): CompatibilityResult =
        get(
            path = "handshake",
            serializer = HandshakeResponse.serializer(),
        ).checkCompatibility(requiredCapabilities)

    public fun listAgents(): List<AgentSummary> =
        get(
            path = "agents/list",
            serializer = ListSerializer(AgentListEntry.serializer()),
        ).map(AgentListEntry::toAgentSummary)

    private fun <T> get(
        path: String,
        serializer: KSerializer<T>,
    ): T {
        val requestBuilder = Request.Builder().url(endpoint(path)).get()
        bearerTokenProvider.token()?.takeIf(String::isNotBlank)?.let { token ->
            requestBuilder.header("Authorization", "Bearer $token")
        }

        val response =
            try {
                httpClient.newCall(requestBuilder.build()).execute()
            } catch (error: IOException) {
                throw HolonProtocolException("Holon request failed", error)
            }

        response.use {
            val body = it.body?.string().orEmpty()
            if (!it.isSuccessful) {
                val apiError =
                    runCatching {
                        HolonWire.json.decodeFromString(ErrorResponse.serializer(), body)
                    }.getOrNull()?.toHolonApiError()
                throw HolonHttpException(it.code, apiError)
            }

            return try {
                HolonWire.json.decodeFromString(serializer, body)
            } catch (error: Exception) {
                throw HolonProtocolException(
                    "Holon returned an invalid response for /$path",
                    error,
                )
            }
        }
    }

    private fun endpoint(path: String): HttpUrl =
        checkNotNull(baseUrl.resolve(path)) {
            "Failed to resolve Holon endpoint: $path"
        }

    public companion object {
        private fun defaultHttpClient(): OkHttpClient =
            OkHttpClient.Builder()
                .followRedirects(false)
                .followSslRedirects(false)
                .build()

        internal fun normalizeBaseUrl(value: String): HttpUrl {
            val parsed =
                requireNotNull(value.toHttpUrlOrNull()) {
                    "Holon base URL must be an absolute HTTP(S) URL"
                }
            require(parsed.username.isEmpty() && parsed.password.isEmpty()) {
                "Holon base URL must not contain user information"
            }
            require(parsed.query == null && parsed.fragment == null) {
                "Holon base URL must not contain a query or fragment"
            }
            require(parsed.scheme == "https" || isLoopbackHttp(parsed)) {
                "Holon base URL must use HTTPS except for loopback development"
            }

            val normalizedPath = parsed.encodedPath.trimEnd('/') + "/"
            return parsed.newBuilder().encodedPath(normalizedPath).build()
        }

        private fun isLoopbackHttp(url: HttpUrl): Boolean =
            url.scheme == "http" &&
                (
                    url.host == "localhost" ||
                        url.host == "::1" ||
                        isIpv4Loopback(url.host)
                )

        private fun isIpv4Loopback(host: String): Boolean {
            val octets = host.split('.')
            return octets.size == 4 &&
                octets.first() == "127" &&
                octets.all { octet ->
                    octet.isNotEmpty() &&
                        octet.all(Char::isDigit) &&
                        octet.toIntOrNull() in 0..255
                }
        }
    }
}
