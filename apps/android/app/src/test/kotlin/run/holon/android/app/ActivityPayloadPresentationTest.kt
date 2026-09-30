package run.holon.android.app

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import kotlinx.serialization.json.putJsonArray
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import run.holon.android.sdk.HolonToolExecutionSnapshot

class ActivityPayloadPresentationTest {
    @Test
    fun `command and output are presented as readable blocks`() {
        val detail = snapshot(
            buildJsonObject {
                putJsonObject("input") { put("cmd", "cargo test --all") }
                putJsonObject("output") {
                    putJsonObject("result") {
                        put("stdout_preview", "running 3 tests\nok")
                        put("exit_status", 0)
                    }
                }
            },
        )

        val blocks = activityPayloadBlocks(detail)

        assertEquals(listOf(ui("命令"), ui("标准输出"), ui("退出状态")), blocks.map { it.title })
        assertEquals("cargo test --all", blocks[0].text)
        assertEquals("running 3 tests\nok", blocks[1].text)
        assertTrue(blocks[0].code)
    }

    @Test
    fun `batch commands, errors, and generic structured payload stay readable`() {
        val detail = snapshot(
            buildJsonObject {
                putJsonObject("input") {
                    putJsonArray("exec_command_batch_items") {
                        add(buildJsonObject { put("cmd_display", "pwd") })
                        add(buildJsonObject { put("cmd", "rg -n TODO src") })
                    }
                }
                putJsonObject("output") {
                    put("error", "command failed")
                    put("exit_status", 2)
                }
            },
        )

        val blocks = activityPayloadBlocks(detail)

        assertEquals("1. pwd\n2. rg -n TODO src", blocks.first().text)
        assertTrue(blocks.any { it.title == ui("错误") && it.text == "command failed" })
        assertTrue(blocks.any { it.title == ui("退出状态") && it.text == "2" })
    }

    @Test
    fun `assistant transcript only exposes text blocks`() {
        val raw = """{"blocks":[{"type":"thinking","text":"secret"},{"type":"text","text":"可见正文"},{"type":"signature","text":"hidden"}]}"""

        assertEquals("可见正文", assistantActivityText(raw))
    }

    private fun snapshot(raw: kotlinx.serialization.json.JsonObject) =
        HolonToolExecutionSnapshot(
            toolExecutionId = "tool-1",
            toolName = "ExecCommand",
            status = "completed",
            summary = null,
            artifactCount = 0,
            raw = raw,
        )
}
