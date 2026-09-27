package run.holon.android.app

import android.content.Context
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.Index
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.Transaction
import androidx.room.migration.Migration
import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.coroutines.flow.first

private val Context.holonDataStore by preferencesDataStore(name = "holon_connection")

internal data class SavedConnection(
    val baseUrl: String,
    val runtimeId: String,
    val userId: String,
    val visibilityScopeId: String,
)

internal class HostPreferences(private val context: Context) {
    private val baseUrlKey = stringPreferencesKey("base_url")
    private val runtimeIdKey = stringPreferencesKey("runtime_id")
    private val userIdKey = stringPreferencesKey("user_id")
    private val visibilityScopeIdKey = stringPreferencesKey("visibility_scope_id")

    suspend fun read(): SavedConnection? {
        val values = context.holonDataStore.data.first()
        val baseUrl = values[baseUrlKey] ?: return null
        val runtimeId = values[runtimeIdKey] ?: return null
        val userId = values[userIdKey] ?: return null
        val visibilityScopeId = values[visibilityScopeIdKey] ?: return null
        return SavedConnection(baseUrl, runtimeId, userId, visibilityScopeId)
    }

    suspend fun write(connection: SavedConnection) {
        context.holonDataStore.edit { values ->
            values[baseUrlKey] = connection.baseUrl
            values[runtimeIdKey] = connection.runtimeId
            values[userIdKey] = connection.userId
            values[visibilityScopeIdKey] = connection.visibilityScopeId
        }
    }

    suspend fun clear() {
        context.holonDataStore.edit { it.clear() }
    }
}

@Entity(
    tableName = "runtime_scope",
    primaryKeys = ["scopeId"],
    indices = [Index(value = ["runtimeId", "userId", "visibilityScopeId"], unique = true)],
)
internal data class RuntimeScopeEntity(
    val scopeId: String,
    val baseUrl: String,
    val runtimeId: String,
    val userId: String,
    val visibilityScopeId: String,
    val createdAt: Long,
    val lastSeenAt: Long,
)

@Entity(
    tableName = "agent_projection",
    primaryKeys = ["scopeKey", "agentId"],
    indices = [Index(value = ["scopeKey", "updatedAt"])],
)
internal data class AgentProjectionEntity(
    val scopeKey: String,
    val agentId: String,
    val displayName: String,
    val posture: String,
    val waitingReason: String?,
    val pending: Int,
    val latestBriefId: String?,
    val latestBriefPreview: String?,
    val latestActivityAt: String?,
    val snapshotJson: String?,
    val updatedAt: Long,
)

@Entity(tableName = "drafts", primaryKeys = ["scopeKey", "agentId"])
internal data class DraftEntity(
    val scopeKey: String,
    val agentId: String,
    val text: String,
    val updatedAt: Long,
)

@Entity(tableName = "composer_attachments", primaryKeys = ["scopeKey", "agentId"])
internal data class ComposerAttachmentsEntity(
    val scopeKey: String,
    val agentId: String,
    val attachmentsJson: String,
    val updatedAt: Long,
)

@Entity(
    tableName = "outbox",
    indices = [Index(value = ["scopeKey", "state", "createdAt"])],
)
internal data class OutboxEntity(
    @androidx.room.PrimaryKey val requestId: String,
    val scopeKey: String,
    val agentId: String,
    val text: String,
    val attachmentsJson: String,
    val state: String,
    val messageId: String?,
    val error: String?,
    val createdAt: Long,
    val updatedAt: Long,
)

@Entity(tableName = "brief_cache", primaryKeys = ["scopeKey", "agentId", "briefId"])
internal data class BriefCacheEntity(
    val scopeKey: String,
    val agentId: String,
    val briefId: String,
    val payloadJson: String,
    val createdAt: String,
)

@Entity(tableName = "read_cursors", primaryKeys = ["scopeKey", "agentId"])
internal data class ReadCursorEntity(
    val scopeKey: String,
    val agentId: String,
    val cursor: String,
    val updatedAt: Long,
)

@Entity(
    tableName = "agent_sync_state",
    primaryKeys = ["scopeKey", "agentId"],
    indices = [Index(value = ["scopeKey", "updatedAt"])],
)
internal data class AgentSyncStateEntity(
    val scopeKey: String,
    val agentId: String,
    val eventCursor: Long?,
    val conversationCursor: String?,
    val eventLogEpoch: String?,
    val updatedAt: Long,
)

@Dao
internal interface HolonDao {
    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putRuntimeScope(entry: RuntimeScopeEntity)

    @Query("SELECT * FROM runtime_scope WHERE scopeId = :scopeId")
    suspend fun runtimeScope(scopeId: String): RuntimeScopeEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putConversations(entries: List<AgentProjectionEntity>)

    @Query("SELECT * FROM agent_projection WHERE scopeKey = :scopeKey ORDER BY updatedAt DESC")
    suspend fun conversations(scopeKey: String): List<AgentProjectionEntity>

    @Query("SELECT * FROM agent_projection WHERE scopeKey = :scopeKey AND agentId = :agentId")
    suspend fun conversation(scopeKey: String, agentId: String): AgentProjectionEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putConversation(entry: AgentProjectionEntity)

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putDraft(draft: DraftEntity)

    @Query("SELECT text FROM drafts WHERE scopeKey = :scopeKey AND agentId = :agentId")
    suspend fun draft(scopeKey: String, agentId: String): String?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putComposerAttachments(entry: ComposerAttachmentsEntity)

    @Query("SELECT attachmentsJson FROM composer_attachments WHERE scopeKey = :scopeKey AND agentId = :agentId")
    suspend fun composerAttachments(scopeKey: String, agentId: String): String?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putOutbox(entry: OutboxEntity)

    @Query("SELECT * FROM outbox WHERE scopeKey = :scopeKey AND agentId = :agentId ORDER BY createdAt")
    suspend fun outbox(scopeKey: String, agentId: String): List<OutboxEntity>

    @Query("SELECT * FROM outbox WHERE scopeKey = :scopeKey AND state IN ('pending', 'sending', 'unknown') ORDER BY createdAt")
    suspend fun pendingOutbox(scopeKey: String): List<OutboxEntity>

    @Query("DELETE FROM outbox WHERE requestId = :requestId")
    suspend fun deleteOutbox(requestId: String)

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putBrief(entry: BriefCacheEntity)

    @Query("SELECT * FROM brief_cache WHERE scopeKey = :scopeKey AND agentId = :agentId AND briefId = :briefId")
    suspend fun brief(scopeKey: String, agentId: String, briefId: String): BriefCacheEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putReadCursor(entry: ReadCursorEntity)

    @Query("SELECT * FROM read_cursors WHERE scopeKey = :scopeKey")
    suspend fun readCursors(scopeKey: String): List<ReadCursorEntity>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putSyncState(entry: AgentSyncStateEntity)

    @Query("SELECT * FROM agent_sync_state WHERE scopeKey = :scopeKey AND agentId = :agentId")
    suspend fun syncState(scopeKey: String, agentId: String): AgentSyncStateEntity?

    @Query("SELECT * FROM agent_sync_state WHERE scopeKey = :scopeKey")
    suspend fun syncStates(scopeKey: String): List<AgentSyncStateEntity>

    @Transaction
    suspend fun putRosterAndSync(
        scope: RuntimeScopeEntity,
        projections: List<AgentProjectionEntity>,
        syncStates: List<AgentSyncStateEntity>,
    ) {
        putRuntimeScope(scope)
        putConversations(projections)
        for (syncState in syncStates) {
            putSyncState(syncState)
        }
    }

    @Transaction
    suspend fun putProjectionAndSync(
        projection: AgentProjectionEntity,
        syncState: AgentSyncStateEntity,
    ) {
        putConversation(projection)
        putSyncState(syncState)
    }

    @Query("DELETE FROM agent_projection WHERE scopeKey != :scopeKey")
    suspend fun purgeOtherConversationScopes(scopeKey: String)

    @Query("DELETE FROM drafts WHERE scopeKey != :scopeKey")
    suspend fun purgeOtherDraftScopes(scopeKey: String)

    @Query("DELETE FROM composer_attachments WHERE scopeKey != :scopeKey")
    suspend fun purgeOtherComposerScopes(scopeKey: String)

    @Query("DELETE FROM outbox WHERE scopeKey != :scopeKey")
    suspend fun purgeOtherOutboxScopes(scopeKey: String)

    @Query("DELETE FROM brief_cache WHERE scopeKey != :scopeKey")
    suspend fun purgeOtherBriefScopes(scopeKey: String)

    @Query("DELETE FROM read_cursors WHERE scopeKey != :scopeKey")
    suspend fun purgeOtherCursorScopes(scopeKey: String)

    @Query("DELETE FROM agent_sync_state WHERE scopeKey != :scopeKey")
    suspend fun purgeOtherSyncScopes(scopeKey: String)

    @Query("DELETE FROM runtime_scope")
    suspend fun clearRuntimeScopes()

    @Query("DELETE FROM agent_projection")
    suspend fun clearConversations()

    @Query("DELETE FROM drafts")
    suspend fun clearDrafts()

    @Query("DELETE FROM composer_attachments")
    suspend fun clearComposerAttachments()

    @Query("DELETE FROM outbox")
    suspend fun clearOutbox()

    @Query("DELETE FROM brief_cache")
    suspend fun clearBriefs()

    @Query("DELETE FROM read_cursors")
    suspend fun clearCursors()

    @Query("DELETE FROM agent_sync_state")
    suspend fun clearSyncStates()
}

@Database(
    entities = [
        RuntimeScopeEntity::class,
        AgentProjectionEntity::class,
        DraftEntity::class,
        ComposerAttachmentsEntity::class,
        OutboxEntity::class,
        BriefCacheEntity::class,
        ReadCursorEntity::class,
        AgentSyncStateEntity::class,
    ],
    version = 3,
    exportSchema = false,
)
internal abstract class HolonDatabase : RoomDatabase() {
    abstract fun holonDao(): HolonDao

    companion object {
        private val MIGRATION_1_2 = object : Migration(1, 2) {
            override fun migrate(db: SupportSQLiteDatabase) {
                db.execSQL("CREATE TABLE IF NOT EXISTS `composer_attachments` (`scopeKey` TEXT NOT NULL, `agentId` TEXT NOT NULL, `attachmentsJson` TEXT NOT NULL, `updatedAt` INTEGER NOT NULL, PRIMARY KEY(`scopeKey`, `agentId`))")
            }
        }

        private val MIGRATION_2_3 = object : Migration(2, 3) {
            override fun migrate(db: SupportSQLiteDatabase) {
                db.execSQL(
                    "CREATE TABLE IF NOT EXISTS `runtime_scope` (" +
                        "`scopeId` TEXT NOT NULL, `baseUrl` TEXT NOT NULL, `runtimeId` TEXT NOT NULL, " +
                        "`userId` TEXT NOT NULL, `visibilityScopeId` TEXT NOT NULL, `createdAt` INTEGER NOT NULL, " +
                        "`lastSeenAt` INTEGER NOT NULL, PRIMARY KEY(`scopeId`))",
                )
                db.execSQL(
                    "CREATE UNIQUE INDEX IF NOT EXISTS `index_runtime_scope_runtimeId_userId_visibilityScopeId` " +
                        "ON `runtime_scope` (`runtimeId`, `userId`, `visibilityScopeId`)",
                )
                db.execSQL(
                    "CREATE TABLE IF NOT EXISTS `agent_projection` (" +
                        "`scopeKey` TEXT NOT NULL, `agentId` TEXT NOT NULL, `displayName` TEXT NOT NULL, " +
                        "`posture` TEXT NOT NULL, `waitingReason` TEXT, `pending` INTEGER NOT NULL, " +
                        "`latestBriefId` TEXT, `latestBriefPreview` TEXT, `latestActivityAt` TEXT, " +
                        "`snapshotJson` TEXT, `updatedAt` INTEGER NOT NULL, PRIMARY KEY(`scopeKey`, `agentId`))",
                )
                db.execSQL(
                    "CREATE INDEX IF NOT EXISTS `index_agent_projection_scopeKey_updatedAt` " +
                        "ON `agent_projection` (`scopeKey`, `updatedAt`)",
                )
                db.execSQL(
                    "CREATE INDEX IF NOT EXISTS `index_outbox_scopeKey_state_createdAt` " +
                        "ON `outbox` (`scopeKey`, `state`, `createdAt`)",
                )
                db.execSQL(
                    "INSERT INTO `agent_projection` " +
                        "SELECT `scopeKey`, `agentId`, `displayName`, `posture`, `waitingReason`, `pending`, " +
                        "`latestBriefId`, `latestBriefPreview`, `latestActivityAt`, `snapshotJson`, `updatedAt` " +
                        "FROM `conversation_cache`",
                )
                db.execSQL(
                    "CREATE TABLE IF NOT EXISTS `agent_sync_state` (" +
                        "`scopeKey` TEXT NOT NULL, `agentId` TEXT NOT NULL, `eventCursor` INTEGER, " +
                        "`conversationCursor` TEXT, `eventLogEpoch` TEXT, `updatedAt` INTEGER NOT NULL, " +
                        "PRIMARY KEY(`scopeKey`, `agentId`))",
                )
                db.execSQL(
                    "CREATE INDEX IF NOT EXISTS `index_agent_sync_state_scopeKey_updatedAt` " +
                        "ON `agent_sync_state` (`scopeKey`, `updatedAt`)",
                )
                db.execSQL("DROP TABLE `conversation_cache`")
            }
        }

        fun create(context: Context): HolonDatabase =
            Room.databaseBuilder(context, HolonDatabase::class.java, "holon.db")
                .addMigrations(MIGRATION_1_2)
                .addMigrations(MIGRATION_2_3)
                .build()
    }
}
