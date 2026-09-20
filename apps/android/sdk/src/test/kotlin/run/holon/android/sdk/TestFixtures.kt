package run.holon.android.sdk

internal fun fixture(name: String): String =
    requireNotNull(object {}.javaClass.getResource("/$name")) {
        "missing fixture: $name"
    }.readText()
