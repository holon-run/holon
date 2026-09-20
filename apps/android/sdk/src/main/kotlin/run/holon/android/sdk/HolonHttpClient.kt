package run.holon.android.sdk

import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.serialization.KSerializer
import kotlinx.serialization.builtins.ListSerializer
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import run.holon.client.wire.generated.models.AgentListEntry
import run.holon.client.wire.generated.models.ErrorResponse
import run.holon.client.wire.generated.models.HandshakeResponse
import run.holon.client.wire.generated.models.NativeSessionResponse
import run.holon.client.wire.generated.models.SessionExchangeRequest

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

public data class SessionCredentials(
    public val credential: String,
    public val userId: String,
    public val expiresAt: String?,
)

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
    private val sessionCredentialStore: SessionCredentialStore?,
) {
    public constructor(
        baseUrl: String,
        bearerTokenProvider: BearerTokenProvider = BearerTokenProvider { null },
        sessionCredentialStore: SessionCredentialStore? = null,
    ) : this(
        baseUrl = baseUrl,
        bearerTokenProvider = bearerTokenProvider,
        httpClient = defaultHttpClient(),
        sessionCredentialStore = sessionCredentialStore,
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

    /**
     * Exchanges a bootstrap/control credential for a revocable session credential.
     *
     * When [sessionCredentialStore] was supplied to the client, the returned
     * credential is persisted through that interface only after a valid response.
     */
    public fun exchangeSession(credential: String): SessionCredentials {
        require(credential.isNotBlank()) { "Session exchange credential must not be blank" }
        val response =
            post(
                path = "auth/session/exchange/native",
                body = SessionExchangeRequest(credential = credential),
                bodySerializer = SessionExchangeRequest.serializer(),
                responseSerializer = NativeSessionResponse.serializer(),
            )
        val session =
            SessionCredentials(
                credential = response.credential,
                userId = response.userId,
                expiresAt = response.expiresAt,
            )
        sessionCredentialStore?.write(session.credential)
        return session
    }

    /**
     * Revokes the current session and clears the platform-provided credential store.
     */
    public fun logout() {
        try {
            postNoContent(
                path = "auth/session/logout",
                preferStoredSession = true,
            )
        } finally {
            sessionCredentialStore?.clear()
        }
    }

    private fun <T> get(
        path: String,
        serializer: KSerializer<T>,
    ): T {
        try {
            val response = httpClient.newCall(authorizedRequest(path).get().build()).execute()
            response.use {
                val body = it.body?.string().orEmpty()
                if (!it.isSuccessful) {
                    throw httpException(it.code, body)
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
        } catch (error: IOException) {
            if (error is HolonHttpException || error is HolonProtocolException) {
                throw error
            }
            throw HolonProtocolException("Holon request failed", error)
        }
    }

    private fun <Request, Response> post(
        path: String,
        body: Request,
        bodySerializer: KSerializer<Request>,
        responseSerializer: KSerializer<Response>,
    ): Response {
        val requestBody =
            HolonWire.json
                .encodeToString(bodySerializer, body)
                .toRequestBody("application/json".toMediaType())
        try {
            val response =
                httpClient
                    .newCall(authorizedRequest(path).post(requestBody).build())
                    .execute()
            response.use {
                val responseText = it.body?.string().orEmpty()
                if (!it.isSuccessful) {
                    throw httpException(it.code, responseText)
                }
                return try {
                    HolonWire.json.decodeFromString(responseSerializer, responseText)
                } catch (error: Exception) {
                    throw HolonProtocolException(
                        "Holon returned an invalid response for /$path",
                        error,
                    )
                }
            }
        } catch (error: IOException) {
            if (error is HolonHttpException || error is HolonProtocolException) {
                throw error
            }
            throw HolonProtocolException("Holon request failed", error)
        }
    }

    private fun postNoContent(
        path: String,
        preferStoredSession: Boolean = false,
    ) {
        try {
            val response =
                httpClient
                    .newCall(
                        authorizedRequest(path, preferStoredSession = preferStoredSession)
                            .post(ByteArray(0).toRequestBody(null))
                            .build(),
                    )
                    .execute()
            response.use {
                val body = it.body?.string().orEmpty()
                if (!it.isSuccessful) {
                    throw httpException(it.code, body)
                }
            }
        } catch (error: IOException) {
            if (error is HolonHttpException || error is HolonProtocolException) {
                throw error
            }
            throw HolonProtocolException("Holon request failed", error)
        }
    }

    private fun authorizedRequest(
        path: String,
        preferStoredSession: Boolean = false,
    ): Request.Builder {
        val requestBuilder = Request.Builder().url(endpoint(path))
        val storedSessionCredential = sessionCredentialStore?.read()?.takeIf(String::isNotBlank)
        val providerCredential = bearerTokenProvider.token()?.takeIf(String::isNotBlank)
        (if (preferStoredSession) {
            storedSessionCredential ?: providerCredential
        } else {
            providerCredential ?: storedSessionCredential
        })
            ?.let { token ->
                requestBuilder.header("Authorization", "Bearer $token")
            }
        return requestBuilder
    }

    private fun httpException(statusCode: Int, body: String): HolonHttpException {
        val apiError =
            runCatching {
                HolonWire.json.decodeFromString(ErrorResponse.serializer(), body)
            }.getOrNull()?.toHolonApiError()
        if (statusCode == 401) {
            sessionCredentialStore?.clear()
        }
        return HolonHttpException(statusCode, apiError)
    }

    private fun endpoint(path: String): HttpUrl =
        checkNotNull(baseUrl.resolve(path)) {
            "Failed to resolve Holon endpoint: $path"
        }

    public companion object {
        internal fun defaultHttpClient(): OkHttpClient =
            OkHttpClient.Builder()
                .callTimeout(30, TimeUnit.SECONDS)
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
                        (octet.length == 1 || octet.first() != '0') &&
                        octet.toIntOrNull() in 0..255
                }
        }
    }
}
