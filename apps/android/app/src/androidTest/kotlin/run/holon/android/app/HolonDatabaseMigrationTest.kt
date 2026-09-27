package run.holon.android.app

import android.content.Context
import android.database.sqlite.SQLiteDatabase
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.sqlite.db.SupportSQLiteDatabase
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class HolonDatabaseMigrationTest {
    @Test
    fun migratesVersion2ToVersion3AndPreservesRosterProjection() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val databaseName = "holon.db"
        context.deleteDatabase(databaseName)

        val legacy = context.openOrCreateDatabase(databaseName, Context.MODE_PRIVATE, null)
        createVersion2Schema(legacy)
        legacy.execSQL(
            """
            INSERT INTO conversation_cache (
                scopeKey, agentId, displayName, posture, waitingReason, pending,
                latestBriefId, latestBriefPreview, latestActivityAt, snapshotJson, updatedAt
            ) VALUES (
                'scope-1', 'agent-1', 'Agent One', 'working', NULL, 1,
                'brief-1', 'Latest brief', '2026-09-27T00:00:00Z', '{"turns":[]}', 42
            )
            """.trimIndent(),
        )
        legacy.execSQL("PRAGMA user_version = 2")
        legacy.close()

        val upgraded = HolonDatabase.create(context)
        val database = upgraded.openHelper.writableDatabase

        assertEquals(3, database.version)
        assertTrue(database.hasTable("runtime_scope"))
        assertTrue(database.hasTable("agent_projection"))
        assertTrue(database.hasTable("agent_sync_state"))
        assertTrue(database.hasIndex("index_outbox_scopeKey_state_createdAt"))

        database.query(
            "SELECT scopeKey, agentId, displayName, pending FROM agent_projection",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals("scope-1", cursor.getString(cursor.getColumnIndexOrThrow("scopeKey")))
            assertEquals("agent-1", cursor.getString(cursor.getColumnIndexOrThrow("agentId")))
            assertEquals("Agent One", cursor.getString(cursor.getColumnIndexOrThrow("displayName")))
            assertEquals(1, cursor.getInt(cursor.getColumnIndexOrThrow("pending")))
        }

        upgraded.close()
        context.deleteDatabase(databaseName)
    }

    private fun createVersion2Schema(database: SQLiteDatabase) {
        database.execSQL(
            """
            CREATE TABLE conversation_cache (
                scopeKey TEXT NOT NULL,
                agentId TEXT NOT NULL,
                displayName TEXT NOT NULL,
                posture TEXT NOT NULL,
                waitingReason TEXT,
                pending INTEGER NOT NULL,
                latestBriefId TEXT,
                latestBriefPreview TEXT,
                latestActivityAt TEXT,
                snapshotJson TEXT,
                updatedAt INTEGER NOT NULL,
                PRIMARY KEY(scopeKey, agentId)
            )
            """.trimIndent(),
        )
        database.execSQL(
            """
            CREATE TABLE drafts (
                scopeKey TEXT NOT NULL,
                agentId TEXT NOT NULL,
                text TEXT NOT NULL,
                updatedAt INTEGER NOT NULL,
                PRIMARY KEY(scopeKey, agentId)
            )
            """.trimIndent(),
        )
        database.execSQL(
            """
            CREATE TABLE composer_attachments (
                scopeKey TEXT NOT NULL,
                agentId TEXT NOT NULL,
                attachmentsJson TEXT NOT NULL,
                updatedAt INTEGER NOT NULL,
                PRIMARY KEY(scopeKey, agentId)
            )
            """.trimIndent(),
        )
        database.execSQL(
            """
            CREATE TABLE outbox (
                requestId TEXT NOT NULL PRIMARY KEY,
                scopeKey TEXT NOT NULL,
                agentId TEXT NOT NULL,
                text TEXT NOT NULL,
                attachmentsJson TEXT NOT NULL,
                state TEXT NOT NULL,
                messageId TEXT,
                error TEXT,
                createdAt INTEGER NOT NULL,
                updatedAt INTEGER NOT NULL
            )
            """.trimIndent(),
        )
        database.execSQL(
            """
            CREATE TABLE brief_cache (
                scopeKey TEXT NOT NULL,
                agentId TEXT NOT NULL,
                briefId TEXT NOT NULL,
                payloadJson TEXT NOT NULL,
                createdAt TEXT NOT NULL,
                PRIMARY KEY(scopeKey, agentId, briefId)
            )
            """.trimIndent(),
        )
        database.execSQL(
            """
            CREATE TABLE read_cursors (
                scopeKey TEXT NOT NULL,
                agentId TEXT NOT NULL,
                cursor TEXT NOT NULL,
                updatedAt INTEGER NOT NULL,
                PRIMARY KEY(scopeKey, agentId)
            )
            """.trimIndent(),
        )
    }

    private fun SupportSQLiteDatabase.hasTable(name: String): Boolean =
        query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
            arrayOf(name),
        ).use { it.moveToFirst() }

    private fun SupportSQLiteDatabase.hasIndex(name: String): Boolean =
        query(
            "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?",
            arrayOf(name),
        ).use { it.moveToFirst() }
}
