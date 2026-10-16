package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotEquals
import kotlin.test.assertNull
import kotlinx.serialization.json.Json
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonJsonDocument
import run.holon.android.sdk.HolonServerInfo
import run.holon.android.sdk.HolonServerLimits

class HolonRepositoryTest {
    @Test
    fun `offline sends have a dedicated retryable error message`() {
        val error = OfflineSessionException()
        assertEquals("当前离线，消息已保存，恢复连接后自动重试", humanError(error))
        assertEquals(true, error.isTransientNetworkFailure())
    }

    @Test
    fun `offline cached session accepts text and attachments without handshake limits`() {
        val session = promptSession().copy(isOffline = true)
        val attachment = StagedAttachment("file", "notes.txt", "text/plain", "/unused", 128)

        session.validatePromptBody("queued offline", emptyList(), "text-request")
        session.validatePromptBody("with file", listOf(attachment), "file-request")
        assertNull(session.attachmentLimit("file"))
        assertNull(session.attachmentLimit("image"))
        assertFailsWith<IllegalArgumentException> { session.attachmentLimit("unsupported") }
    }

    @Test
    fun `online session without limits still requires daemon upgrade`() {
        val session = promptSession()

        assertEquals(
            "daemon 未报告消息大小限制，请升级 daemon",
            assertFailsWith<IllegalArgumentException> {
                session.validatePromptBody("hello", emptyList(), "request")
            }.message,
        )
        assertEquals(
            "daemon 未报告附件大小限制，请升级 daemon",
            assertFailsWith<IllegalArgumentException> { session.attachmentLimit("file") }.message,
        )
    }

    @Test
    fun `online size limits still apply at the encoded body boundary`() {
        val attachment = StagedAttachment("image", "photo.png", "image/png", "/unused", 3)
        val bodySize = encodedPromptBodySize("hello", listOf(attachment), "request")
        val session = promptSession(HolonServerLimits(bodySize, 8, 3))

        session.validatePromptBody("hello", listOf(attachment), "request")
        assertFailsWith<IllegalArgumentException> {
            session.validatePromptBody("hello!", listOf(attachment), "request")
        }
        assertEquals(8L, session.attachmentLimit("file"))
        assertEquals(3L, session.attachmentLimit("image"))
    }

    private fun promptSession(limits: HolonServerLimits? = null) =
        ActiveSession(
            "network", "https://example.test/api/", HolonCurrentUser("user", null, "session"),
            "runtime", "visibility", HolonServerInfo("", "local", true, emptySet(), limits),
        )

    @Test
    fun `conversation cache keeps loaded older turns while using incoming pagination state`() {
        val cached = snapshot(
            """{"runtime_id":"runtime-1","event_log_epoch":"epoch-1","turns":[{"turn_id":"new","started_at":"2026-09-27T10:00:00Z"}],"pending_inputs":[]}""",
        )
        val incoming = snapshot(
            """{"runtime_id":"runtime-1","event_log_epoch":"epoch-1","turns":[{"turn_id":"old","started_at":"2026-09-27T09:00:00Z"}],"pending_inputs":[],"has_more":true,"next_before_cursor":"cursor-1"}""",
        )

        val merged = mergeConversationSnapshots(cached, incoming)

        assertEquals(listOf("old", "new"), merged.turns.map { it.id })
        assertEquals(true, merged.hasMore)
        assertEquals("cursor-1", merged.nextBeforeCursor)
    }

    @Test
    fun `conversation cache is not merged across runtime epochs`() {
        val cached =
            snapshot(
                """{"runtime_id":"runtime-1","event_log_epoch":"epoch-1","turns":[{"turn_id":"cached","started_at":"2026-09-27T10:00:00Z"}],"pending_inputs":[]}""",
            )
        val incoming =
            snapshot(
                """{"runtime_id":"runtime-1","event_log_epoch":"epoch-2","turns":[{"turn_id":"incoming","started_at":"2026-09-27T11:00:00Z"}],"pending_inputs":[]}""",
            )

        val merged = mergeConversationSnapshots(cached, incoming)

        assertEquals(listOf("incoming"), merged.turns.map { it.id })
        assertNotEquals(cached.eventLogEpoch, merged.eventLogEpoch)
    }

    @Test
    fun `network profiles preserve independent identity and connection settings`() {
        val profiles =
            listOf(
                NetworkProfile(
                    networkId = "network-a",
                    displayName = "Office",
                    baseUrl = "https://office.example/api/",
                    allowInsecureHttp = false,
                ),
                NetworkProfile(
                    networkId = "network-b",
                    displayName = "Lab",
                    baseUrl = "http://10.0.2.2:7878/api/",
                    allowInsecureHttp = true,
                ),
            )
        val serializer =
            kotlinx.serialization.builtins.ListSerializer(NetworkProfile.serializer())
        val encoded = Json.encodeToString(serializer, profiles)
        val decoded = Json.decodeFromString(serializer, encoded)

        assertEquals(profiles, decoded)
        assertNotEquals(decoded[0].networkId, decoded[1].networkId)
        assertEquals("https://office.example/api/", decoded[0].baseUrl)
        assertEquals(true, decoded[1].allowInsecureHttp)
    }

    @Test
    fun `conversation cache discards data from another runtime scope`() {
        val cached = snapshot(
            """{"runtime_id":"runtime-old","event_log_epoch":"epoch-1","turns":[{"turn_id":"old"}]}""",
        )
        val incoming = snapshot(
            """{"runtime_id":"runtime-new","event_log_epoch":"epoch-1","turns":[{"turn_id":"new"}]}""",
        )

        val merged = mergeConversationSnapshots(cached, incoming)

        assertEquals(listOf("new"), merged.turns.map { it.id })
    }

    @Test
    fun `pairing auth failure names the one-time code instead of an expired session`() {
        assertEquals(
            "配对码无效或已过期，请在 macOS 菜单重新生成",
            pairingHumanError(HolonHttpException(401, null)),
        )
        assertEquals(
            "无法连接 Holon 主机，请确认 daemon 已启动",
            pairingHumanError(java.net.ConnectException("refused")),
        )
    }

    private fun snapshot(raw: String): HolonConversationSnapshot =
        HolonConversationSnapshot.from(
            HolonJsonDocument(Json.parseToJsonElement(raw)),
        )
}
