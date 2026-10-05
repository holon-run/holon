package run.holon.android.app

/** Includes seed/bootstrap work, not only the blocking event reader. */
internal suspend fun <T> withOwnedConnection(connection: AutoCloseable, read: suspend () -> T): T =
    try { read() } finally { connection.close() }
