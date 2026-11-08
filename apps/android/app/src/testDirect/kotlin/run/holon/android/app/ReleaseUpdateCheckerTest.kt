package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs

class ReleaseUpdateCheckerTest {
    private val checker = ReleaseUpdateChecker()

    @Test
    fun `selects highest valid stable Android release`() {
        val payload = """
            [
              ${release("1.3.0", "https://example.test/release-130", "metadata-130")},
              ${release("1.4.0", "https://example.test/release-140", "metadata-140", prerelease = true)},
              ${release("1.2.0", "https://example.test/release-120", "metadata-120")}
            ]
        """.trimIndent()

        val result = checker.parse(
            payload = payload,
            currentVersionCode = 1_000_000,
            metadataByUrl = mapOf(
                "metadata-130" to metadata("1.3.0"),
                "metadata-120" to metadata("1.2.0"),
            ),
        )

        val available = assertIs<ReleaseUpdateResult.Available>(result)
        assertEquals("1.3.0", available.release.version)
        assertEquals("https://example.test/release-130", available.release.releaseUrl)
    }

    @Test
    fun `ignores stable releases with invalid Android metadata`() {
        val result = checker.parse(
            payload = "[${release("1.3.0", "https://example.test/release-130", "metadata-130")}]",
            currentVersionCode = 1_000_000,
            metadataByUrl = mapOf("metadata-130" to metadata("1.3.1")),
        )

        val invalid = assertIs<ReleaseUpdateResult.InvalidRelease>(result)
        assertEquals("Android metadata does not match release 1.3.0", invalid.reason)
    }

    @Test
    fun `returns no update when current version is latest`() {
        val result = checker.parse(
            payload = "[${release("1.3.0", "https://example.test/release-130", "metadata-130")}]",
            currentVersionCode = 1_003_000,
            metadataByUrl = mapOf("metadata-130" to metadata("1.3.0")),
        )

        assertIs<ReleaseUpdateResult.NoUpdate>(result)
    }

    @Test
    fun `ignores drafts prereleases and non semver tags`() {
        val payload = """
            [
              ${release("2.0.0", "https://example.test/release-200", "metadata-200", draft = true)},
              ${release("1.9.0", "https://example.test/release-190", "metadata-190", prerelease = true)},
              {
                "tag_name": "nightly",
                "html_url": "https://example.test/nightly",
                "assets": []
              }
            ]
        """.trimIndent()

        assertIs<ReleaseUpdateResult.NoUpdate>(
            checker.parse(payload, currentVersionCode = 1_000_000),
        )
    }

    @Test
    fun `carries asset browser URL and published integrity metadata`() {
        val result = checker.parse(
            "[${release("1.3.0", "https://example.test/v130", "metadata-130")}]",
            1_000_000,
            mapOf("metadata-130" to metadata("1.3.0").dropLast(1) +
                ""","apk_sha256":"${"a".repeat(64)}","apk_size":12345}"""),
        )
        val apk = assertIs<ReleaseUpdateResult.Available>(result).release
        assertEquals("https://example.test/v130/holon-android-v1.3.0.apk", apk.apkUrl)
        assertEquals("a".repeat(64), apk.apkSha256)
        assertEquals(12345L, apk.apkSize)
    }

    @Test
    fun `legacy metadata remains visible but cannot authorize in-app download`() {
        val result = checker.parse(
            "[${release("1.3.0", "https://example.test/v130", "metadata-130")}]",
            1_000_000,
            mapOf("metadata-130" to metadata("1.3.0")),
        )
        val apk = assertIs<ReleaseUpdateResult.Available>(result).release
        assertEquals("", apk.apkSha256)
        assertEquals(0L, apk.apkSize)
        kotlin.test.assertNotNull(updateIntegrityProblem(apk.apkSize, apk.apkSha256))
    }

    @Test
    fun `malformed release elements and enormous versions fail closed`() {
        assertIs<ReleaseUpdateResult.InvalidRelease>(checker.parse("[false]", 1))
        assertIs<ReleaseUpdateResult.InvalidRelease>(
            checker.parse("[${release("999999999999999999999.0.0", "https://example.test/r", "m")}]", 1),
        )
    }

    @Test
    fun `metadata with wrong package or malformed size is rejected`() {
        val payload = "[${release("1.3.0", "https://example.test/r", "m")}]"
        assertIs<ReleaseUpdateResult.InvalidRelease>(
            checker.parse(payload, 1, mapOf("m" to metadata("1.3.0").replace("run.holon.android", "other.app"))),
        )
        assertIs<ReleaseUpdateResult.InvalidRelease>(
            checker.parse(payload, 1, mapOf("m" to metadata("1.3.0").dropLast(1) + ""","apk_size":"oops"}""")),
        )
    }

    private fun release(
        version: String,
        releaseUrl: String,
        metadataUrl: String,
        draft: Boolean = false,
        prerelease: Boolean = false,
    ): String {
        val metadataName = "holon-android-v$version.json"
        val apkName = "holon-android-v$version.apk"
        return """
            {
              "tag_name": "v$version",
              "draft": $draft,
              "prerelease": $prerelease,
              "html_url": "$releaseUrl",
              "body": "Release $version",
              "assets": [
                {"name": "$metadataName", "browser_download_url": "$metadataUrl"},
                {"name": "$apkName", "browser_download_url": "$releaseUrl/$apkName"}
              ]
            }
        """.trimIndent()
    }

    private fun metadata(version: String): String {
        val parts = version.split('.')
        val versionCode = parts[0].toLong() * 1_000_000 +
            parts[1].toLong() * 1_000 +
            parts[2].toLong()
        return """{"version":"$version","version_code":$versionCode,"package":"run.holon.android","apk":"holon-android-v$version.apk"}"""
    }
}
