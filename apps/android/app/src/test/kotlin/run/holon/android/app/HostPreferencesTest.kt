package run.holon.android.app

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Rule
import org.junit.rules.TemporaryFolder

class HostPreferencesTest {
    @get:Rule val temporaryFolder = TemporaryFolder()

    private fun profile(id: String, used: Long) =
        NetworkProfile(id, id, "https://$id.example/", false, lastUsedAt = used)

    private fun withPreferences(test: suspend (HostPreferences, androidx.datastore.core.DataStore<androidx.datastore.preferences.core.Preferences>) -> Unit) =
        runBlocking {
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
            val store = PreferenceDataStoreFactory.create(scope = scope) {
                temporaryFolder.root.resolve("connection.preferences_pb")
            }
            try {
                test(HostPreferences(store), store)
            } finally {
                scope.cancel()
            }
        }

    @Test fun `removing unselected profile preserves selection and other profiles`() = withPreferences { preferences, _ ->
        preferences.upsertProfile(profile("selected", 1))
        preferences.upsertProfile(profile("recent", 100))
        preferences.upsertProfile(profile("removed", 50))
        preferences.selectProfile("selected")
        preferences.removeProfile("removed")
        assertEquals("selected", preferences.selectedProfile()?.networkId)
        assertEquals(listOf("recent", "selected"), preferences.profiles().map { it.networkId })
    }

    @Test fun `removing selected profile selects most recently used remaining profile`() = withPreferences { preferences, _ ->
        preferences.upsertProfile(profile("old", 1))
        preferences.upsertProfile(profile("recent", 100))
        preferences.upsertProfile(profile("selected", 50))
        preferences.removeProfile("selected")
        assertEquals("recent", preferences.selectedProfile()?.networkId)
    }

    @Test fun `last removal clears legacy fields and remains empty after new reader`() = withPreferences { preferences, store ->
        val legacy = SavedConnection("https://legacy.example/", "runtime", "user", "visibility")
        preferences.write(legacy)
        assertEquals(legacy.networkId, preferences.profiles().single().networkId)
        preferences.removeProfile(legacy.networkId)
        assertNull(preferences.read())
        assertEquals("[]", store.data.first()[stringPreferencesKey("network_profiles")])
        assertEquals(emptyList(), HostPreferences(store).profiles())
        assertNull(HostPreferences(store).selectedProfile())
    }

    @Test fun `explicit empty list never migrates legacy fields`() = withPreferences { preferences, store ->
        preferences.write(SavedConnection("https://legacy.example/", "runtime", "user", "visibility"))
        store.edit { it[stringPreferencesKey("network_profiles")] = "[]" }
        assertEquals(emptyList(), preferences.profiles())
        assertNull(preferences.selectedProfile())
    }

    @Test fun `unknown removal does not migrate or change preferences`() = withPreferences { preferences, store ->
        preferences.write(SavedConnection("https://legacy.example/", "runtime", "user", "visibility"))
        val before = store.data.first()
        preferences.removeProfile("unknown")
        assertEquals(before, store.data.first())
        preferences.upsertProfile(profile("selected", 1))
        val migrated = store.data.first()
        preferences.removeProfile("unknown")
        assertEquals(migrated, store.data.first())
    }

    @Test fun `removing legacy owner clears only its connection fields`() = withPreferences { preferences, store ->
        val legacy = SavedConnection("https://legacy.example/", "runtime", "user", "visibility")
        preferences.write(legacy)
        preferences.profiles()
        preferences.upsertProfile(profile("retained", 100))
        store.edit { it[stringPreferencesKey("unrelated")] = "keep" }
        preferences.removeProfile(legacy.networkId)
        assertNull(preferences.read())
        assertEquals("retained", preferences.selectedProfile()?.networkId)
        assertEquals("keep", store.data.first()[stringPreferencesKey("unrelated")])
    }
}
