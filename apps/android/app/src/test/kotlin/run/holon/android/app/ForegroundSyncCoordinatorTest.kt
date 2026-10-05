package run.holon.android.app

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertEquals

@OptIn(ExperimentalCoroutinesApi::class)
class ForegroundSyncCoordinatorTest {
    @Test fun `100 agents still open one hint connection and foreground stop closes it`() = runTest {
        val dispatcher = StandardTestDispatcher(testScheduler)
        var opens = 0
        var closes = 0
        val hints = mutableListOf<String>()
        val coordinator = ForegroundSyncCoordinator(
            scope = this, open = {
                opens++
                object : RosterHintConnection {
                    override fun hints() = (1..100).map { "agent-$it" }.asSequence()
                    override fun close() { closes++ }
                }
            }, onConnected = {}, onHint = hints::add, onFailure = { throw it }, reader = dispatcher, callbacks = dispatcher,
        )
        repeat(100) { coordinator.start(7) }
        runCurrent()
        assertEquals(1, opens)
        assertEquals(100, hints.size)
        coordinator.stop()
        runCurrent()
        assertEquals(1, closes)
    }
}
