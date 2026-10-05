package run.holon.android.app

import android.content.Context
import android.net.Uri
import java.io.File
import java.io.IOException
import java.util.UUID
import run.holon.android.sdk.HolonDownloadedFile
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonFileReference
import run.holon.android.sdk.HolonFileReferenceResult
import run.holon.android.sdk.HolonWorkItemPlanArtifact
import run.holon.android.sdk.HolonWorkspace
import run.holon.android.sdk.HolonWorkspaceDirectory

private const val MAX_ARTIFACT_CACHE_BYTES = 100L * 1024L * 1024L

/** Platform file I/O is separate from conversation/auth ownership. */
internal class FilesRepository(private val context: Context, private val sessions: SessionCoordinator) {
    suspend fun workspaces(agentId: String): List<HolonWorkspace> =
        sessions.read { it.agentWorkspaces(agentId) }

    suspend fun browseWorkspace(
        workspace: HolonWorkspace,
        path: String = "",
    ): HolonWorkspaceDirectory =
        sessions.read { it.browseWorkspaceDirectory(
            workspaceId = workspace.workspaceId,
            path = path,
            executionRootId = workspace.executionRootId,
        ) }

    suspend fun resolveFileReference(reference: HolonFileReference): HolonFileReferenceResult =
        sessions.read { it.resolveFileReference(reference) }

    suspend fun prepareArtifact(locator: String, preferredName: String): PreparedArtifact {
        return cacheDownloadedArtifact(locator, preferredName) { client, target ->
            client.downloadWorkspaceArtifactToFile(locator, target, MAX_ARTIFACT_CACHE_BYTES)
        }
    }

    suspend fun prepareWorkspaceFile(
        workspace: HolonWorkspace,
        path: String,
    ): PreparedArtifact {
        val sourceKey = listOf(workspace.workspaceId, workspace.executionRootId.orEmpty(), path).joinToString("|")
        return cacheDownloadedArtifact(sourceKey, path.substringAfterLast('/')) { client, target ->
            client.downloadWorkspaceFileToFile(
                workspaceId = workspace.workspaceId,
                path = path,
                targetFile = target,
                maxBytes = MAX_ARTIFACT_CACHE_BYTES,
                executionRootId = workspace.executionRootId,
            )
        }
    }

    suspend fun prepareWorkItemPlan(agentId: String, plan: HolonWorkItemPlanArtifact): PreparedArtifact {
        require(plan.ownerAgentId == null || plan.ownerAgentId == agentId) { "计划不属于当前 Agent" }
        val workspaceId = plan.workspaceId?.takeIf(String::isNotBlank)
            ?: throw IllegalArgumentException("服务端没有提供计划的工作区标识")
        val relativePath = plan.relativePath?.takeIf(String::isNotBlank)
            ?: throw IllegalArgumentException("服务端没有提供计划文件位置")
        return cacheDownloadedArtifact("plan|$workspaceId|$relativePath", "plan.md") { client, target ->
            client.downloadWorkspaceFileToFile(
                workspaceId = workspaceId,
                path = relativePath,
                targetFile = target,
                maxBytes = MAX_ARTIFACT_CACHE_BYTES,
            )
        }
    }

    suspend fun saveArtifactToDevice(artifact: PreparedArtifact, destination: Uri) {
        val source = File(artifact.localPath)
        require(source.isFile) { "预览文件已不存在，请重新读取后再保存" }
        context.contentResolver.openOutputStream(destination, "wt")?.use { output ->
            source.inputStream().use { input -> input.copyTo(output) }
        } ?: throw IOException("无法写入所选位置")
    }

    private fun cacheDownloadedArtifact(
        locator: String,
        preferredName: String,
        download: (HolonHttpClient, File) -> HolonDownloadedFile,
    ): PreparedArtifact {
        val lease = sessions.capture()
        val directory = File(context.cacheDir, "shared-artifacts/${scopeFileKey(lease.session.scopeKey)}").apply { mkdirs() }
        val cachedFiles = directory.listFiles().orEmpty().filter(File::isFile).sortedBy(File::lastModified)
        var cachedBytes = cachedFiles.sumOf(File::length)
        cachedFiles.forEach { cached ->
            if (cachedBytes > MAX_ARTIFACT_CACHE_BYTES * 2) {
                cachedBytes -= cached.length()
                cached.delete()
            }
        }
        val name = safeFileName(preferredName.ifBlank { "artifact" })
        val target = File(directory, "${UUID.randomUUID()}-$name")
        return try {
            val downloaded = download(lease.client, target)
            sessions.requireCurrent(lease)
            trimArtifactCache(directory, protected = target)
            PreparedArtifact(locator, target.absolutePath, downloaded.mediaType, name)
        } catch (error: Throwable) {
            target.delete()
            throw error
        }
    }

}
