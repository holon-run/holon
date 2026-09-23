package run.holon.android.app

import android.content.Context
import run.holon.android.sdk.SessionCredentialStore

internal fun createSessionStore(context: Context): SessionCredentialStore =
    object : SessionCredentialStore {
        override fun read(): String? = null

        override fun write(credential: String) = Unit

        override fun clear() = Unit
    }
