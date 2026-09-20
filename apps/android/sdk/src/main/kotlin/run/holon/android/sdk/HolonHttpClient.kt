package run.holon.android.sdk

import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.serialization.KSerializer
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
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
    private val sseHttpClient: OkHttpClient =
        sseHttpClient(httpClient)

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

    /** Returns a forward-compatible JSON document from a read route. */
    public fun getJson(
        path: String,
        query: Map<String, String> = emptyMap(),
    ): HolonJsonDocument =
        HolonJsonDocument(
            get(
                path = path,
                query = query,
                serializer = JsonElement.serializer(),
            ),
        )

    /** Sends an open JSON request body to a mutation route. */
    public fun postJson(
        path: String,
        body: JsonObject = JsonObject(emptyMap()),
    ): HolonJsonDocument =
        HolonJsonDocument(
            post(
                path = path,
                body = body,
                bodySerializer = JsonElement.serializer(),
                responseSerializer = JsonElement.serializer(),
            ),
        )

    public fun conversation(
        agentId: String,
        limit: Int? = null,
        before: String? = null,
    ): HolonJsonDocument =
        getJson(
            path = "agents/${agentId.pathSegment()}/conversation",
            query =
                buildMap {
                    limit?.let { put("limit", it.toString()) }
                    before?.let { put("before", it) }
                },
        )

    public fun agentState(agentId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/state")

    public fun projectionSnapshot(agentId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/projection-snapshot")

    public fun events(agentId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/events")

    public fun tasks(agentId: String, limit: Int? = null): HolonJsonDocument =
        getJson(
            path = "agents/${agentId.pathSegment()}/tasks",
            query = limit?.let { mapOf("limit" to it.toString()) } ?: emptyMap(),
        )

    public fun taskStatus(agentId: String, taskId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}")

    public fun taskOutput(
        agentId: String,
        taskId: String,
        block: Boolean? = null,
        timeoutMillis: Long? = null,
    ): HolonJsonDocument =
        getJson(
            path = "agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}/output",
            query =
                buildMap {
                    block?.let { put("block", it.toString()) }
                    timeoutMillis?.let { put("timeout_ms", it.toString()) }
                },
        )

    public fun toolExecution(agentId: String, toolExecutionId: String): HolonJsonDocument =
        getJson(
            "agents/${agentId.pathSegment()}/tool-executions/${toolExecutionId.pathSegment()}",
        )

    public fun artifact(
        agentId: String,
        toolExecutionId: String,
        artifactIndex: Int,
    ): HolonArtifact {
        require(artifactIndex >= 0) { "artifactIndex must be non-negative" }
        val objectValue =
            getJson(
                "agents/${agentId.pathSegment()}/tool-executions/" +
                    "${toolExecutionId.pathSegment()}/artifacts/$artifactIndex",
            ).objectOrNull
                ?: throw HolonProtocolException("Holon returned a non-object artifact response")
        val responseArtifactIndex =
            objectValue.int("artifact_index")
                ?: throw HolonProtocolException("Holon artifact response is missing artifact_index")
        if (responseArtifactIndex != artifactIndex) {
            throw HolonProtocolException(
                "Holon artifact response index $responseArtifactIndex does not match requested index $artifactIndex",
            )
        }
        val responseSize =
            objectValue.long("size")
                ?: throw HolonProtocolException("Holon artifact response is missing size")
        if (responseSize < 0) {
            throw HolonProtocolException("Holon artifact response size must be non-negative")
        }
        val responseContent =
            objectValue.string("content")
                ?: throw HolonProtocolException("Holon artifact response is missing content")
        return HolonArtifact(
            artifactIndex = responseArtifactIndex,
            size = responseSize,
            content = responseContent,
        )
    }

    public fun enqueue(agentId: String, body: JsonObject): HolonEnqueueResult {
        val objectValue =
            postJson("agents/${agentId.pathSegment()}/enqueue", body).objectOrNull
                ?: throw HolonProtocolException("Holon returned a non-object enqueue response")
        return HolonEnqueueResult(
            ok = objectValue["ok"]?.toString()?.toBoolean() ?: false,
            agentId = objectValue.string("agent_id") ?: agentId,
            messageId = objectValue.string("message_id"),
            raw = objectValue,
        )
    }

    public fun createCommandTask(agentId: String, body: JsonObject): HolonJsonDocument =
        postJson("control/agents/${agentId.pathSegment()}/tasks", body)

    public fun taskInput(
        agentId: String,
        taskId: String,
        body: JsonObject,
    ): HolonJsonDocument =
        postJson(
            "control/agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}/input",
            body,
        )

    public fun stopTask(
        agentId: String,
        taskId: String,
        body: JsonObject = JsonObject(emptyMap()),
    ): HolonJsonDocument =
        postJson(
            "control/agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}/stop",
            body,
        )

    public fun openSse(
        path: String,
        query: Map<String, String> = emptyMap(),
        lastEventId: String? = null,
        deduplicate: Boolean = true,
    ): HolonSseConnection {
        val request =
            authorizedRequest(path, query = query)
                .header("Accept", "text/event-stream")
                .apply {
                    lastEventId?.takeIf(String::isNotBlank)?.let {
                        header("Last-Event-ID", it)
                    }
                }
                .get()
                .build()
        val response = sseHttpClient.newCall(request).execute()
        if (!response.isSuccessful) {
            val body = response.body?.string().orEmpty()
            val statusCode = response.code
            response.close()
            throw httpException(statusCode, body)
        }
        val body =
            response.body
                ?: run {
                    response.close()
                    throw HolonProtocolException("Holon returned an empty SSE response")
                }
        return HolonSseConnection(
            body = body,
            deduplicator = if (deduplicate) HolonSseDeduplicator() else null,
        )
    }

    public fun conversationStream(
        agentId: String,
        after: String? = null,
        limit: Int? = null,
        activityLimit: Int? = null,
        lastEventId: String? = null,
        deduplicate: Boolean = true,
    ): HolonSseConnection =
        openSse(
            path = "agents/${agentId.pathSegment()}/conversation/stream",
            query =
                buildMap {
                    after?.let { put("after", it) }
                    limit?.let { put("limit", it.toString()) }
                    activityLimit?.let { put("activity_limit", it.toString()) }
                },
            lastEventId = lastEventId,
            deduplicate = deduplicate,
        )

    /**
     * Reconnects after clean disconnects, carrying the last SSE cursor and
     * dropping duplicate frames. The sequence ends after [policy.maxAttempts]
     * consecutive reconnects without a newly emitted event.
     */
    public fun reconnectingConversationStream(
        agentId: String,
        after: String? = null,
        limit: Int? = null,
        activityLimit: Int? = null,
        policy: SseReconnectPolicy = SseReconnectPolicy(),
    ): Sequence<HolonSseEvent> =
        reconnectingSse(
            path = "agents/${agentId.pathSegment()}/conversation/stream",
            query =
                buildMap {
                    after?.let { put("after", it) }
                    limit?.let { put("limit", it.toString()) }
                    activityLimit?.let { put("activity_limit", it.toString()) }
                },
            policy = policy,
        )

    private fun reconnectingSse(
        path: String,
        query: Map<String, String>,
        policy: SseReconnectPolicy,
    ): Sequence<HolonSseEvent> =
        sequence {
            val deduplicator = HolonSseDeduplicator()
            var attempts = 0
            var lastEventId: String? = null
            while (true) {
                var emitted = false
                try {
                    val connection =
                        openSse(
                            path = path,
                            query = query,
                            lastEventId = lastEventId,
                            deduplicate = false,
                        )
                    try {
                        for (event in connection.events()) {
                            lastEventId = event.id ?: lastEventId
                            if (deduplicator.accept(event)) {
                                emitted = true
                                yield(event)
                            }
                        }
                    } finally {
                        connection.close()
                    }
                } catch (error: IOException) {
                    if (!error.isRetryableSseFailure()) {
                        throw error
                    }
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return@sequence
                }
                if (attempts >= policy.maxAttempts) {
                    return@sequence
                }
                val delayMillis =
                    (policy.initialDelayMillis shl attempts.coerceAtMost(20))
                        .coerceAtMost(policy.maxDelayMillis)
                if (delayMillis > 0) {
                    try {
                        Thread.sleep(delayMillis)
                    } catch (_: InterruptedException) {
                        Thread.currentThread().interrupt()
                        return@sequence
                    }
                }
                attempts++
                if (emitted) {
                    attempts = 0
                }
            }
        }

    private fun <T> get(
        path: String,
        query: Map<String, String> = emptyMap(),
        serializer: KSerializer<T>,
    ): T {
        try {
            val response =
                httpClient
                    .newCall(authorizedRequest(path, query = query).get().build())
                    .execute()
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
        query: Map<String, String> = emptyMap(),
    ): Request.Builder {
        val requestBuilder = Request.Builder().url(endpoint(path, query))
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

    private fun endpoint(
        path: String,
        query: Map<String, String> = emptyMap(),
    ): HttpUrl {
        val resolved =
            checkNotNull(baseUrl.resolve(path)) {
                "Failed to resolve Holon endpoint: $path"
            }
        require(resolved.host == baseUrl.host && resolved.port == baseUrl.port) {
            "Holon endpoint must remain on the configured origin"
        }
        return resolved
            .newBuilder()
            .apply {
                query.forEach { (name, value) ->
                    addQueryParameter(name, value)
                }
            }
            .build()
    }

    private fun String.pathSegment(): String {
        require(isNotBlank() && !contains('/')) {
            "Holon path identifiers must be non-blank and must not contain '/'"
        }
        return this
    }

    public companion object {
        internal fun defaultHttpClient(): OkHttpClient =
            OkHttpClient.Builder()
                .callTimeout(30, TimeUnit.SECONDS)
                .followRedirects(false)
                .followSslRedirects(false)
                .build()

        internal fun defaultSseHttpClient(): OkHttpClient =
            sseHttpClient(defaultHttpClient())

        private fun sseHttpClient(httpClient: OkHttpClient): OkHttpClient =
            httpClient
                .newBuilder()
                .callTimeout(0, TimeUnit.MILLISECONDS)
                .readTimeout(45, TimeUnit.SECONDS)
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
