package run.holon.android.app

import android.content.Context
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
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

@Entity(tableName = "conversation_cache", primaryKeys = ["scopeKey", "agentId"])
internal data class ConversationCacheEntity(
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

@Entity(tableName = "outbox")
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

@Dao
internal interface HolonDao {
    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putConversations(entries: List<ConversationCacheEntity>)

    @Query("SELECT * FROM conversation_cache WHERE scopeKey = :scopeKey ORDER BY updatedAt DESC")
    suspend fun conversations(scopeKey: String): List<ConversationCacheEntity>

    @Query("SELECT * FROM conversation_cache WHERE scopeKey = :scopeKey AND agentId = :agentId")
    suspend fun conversation(scopeKey: String, agentId: String): ConversationCacheEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun putConversation(entry: ConversationCacheEntity)

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

    @Query("DELETE FROM conversation_cache WHERE scopeKey != :scopeKey")
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

    @Query("DELETE FROM conversation_cache")
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
}

@Database(
    entities = [
        ConversationCacheEntity::class,
        DraftEntity::class,
        ComposerAttachmentsEntity::class,
        OutboxEntity::class,
        BriefCacheEntity::class,
        ReadCursorEntity::class,
    ],
    version = 2,
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

        fun create(context: Context): HolonDatabase =
            Room.databaseBuilder(context, HolonDatabase::class.java, "holon.db")
                .addMigrations(MIGRATION_1_2)
                .build()
    }
}
