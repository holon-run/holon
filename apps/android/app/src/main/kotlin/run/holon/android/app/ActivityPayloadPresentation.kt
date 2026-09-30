package run.holon.android.app

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import run.holon.android.sdk.HolonToolExecutionSnapshot

internal data class ActivityPayloadBlock(
    val title: String,
    val text: String,
    val code: Boolean = false,
)

private val prettyJson = Json {
    prettyPrint = true
    explicitNulls = false
}

internal fun activityPayloadBlocks(detail: HolonToolExecutionSnapshot): List<ActivityPayloadBlock> {
    val blocks = buildList {
        detail.raw["input"]?.let { addAll(inputBlocks(it)) }
        detail.raw["output"]?.let { addAll(outputBlocks(it)) }
        detail.raw["error"]?.let { add(ActivityPayloadBlock(ui("错误"), displayValue(it))) }
        if (isEmpty() && detail.summary.orEmpty().isNotBlank()) {
            add(ActivityPayloadBlock(ui("结果"), detail.summary.orEmpty()))
        }
    }
    return blocks.ifEmpty {
        listOf(ActivityPayloadBlock(ui("记录"), readableObject(detail.raw)))
    }
}

internal fun assistantActivityText(raw: String): String {
    val element = raw.toJsonElementOrNull() ?: return raw
    val objectValue = element as? JsonObject ?: return raw
    val blocks = objectValue["blocks"] as? JsonArray ?: return raw
    return blocks.mapNotNull { block ->
        val value = block as? JsonObject ?: return@mapNotNull null
        if (value["type"].stringValue() == "text") value["text"].stringValue() else null
    }.joinToString("\n\n").ifBlank { "" }
}

private fun inputBlocks(element: JsonElement): List<ActivityPayloadBlock> {
    val value = decoded(element)
    val objectValue = value as? JsonObject
    if (objectValue != null) {
        val batch = objectValue["exec_command_batch_items"] as? JsonArray
        if (batch != null) {
            val commands = batch.mapNotNull { item ->
                val itemObject = item as? JsonObject ?: return@mapNotNull null
                itemObject.firstString("cmd_display", "cmd", "command")
            }
            if (commands.isNotEmpty()) {
                return listOf(ActivityPayloadBlock(ui("命令"), commands.mapIndexed { index, command -> "${index + 1}. $command" }.joinToString("\n"), code = true))
            }
        }
        val command = objectValue.firstString("exec_command_display", "cmd_display", "cmd", "command", "command_line")
        if (command != null) return listOf(ActivityPayloadBlock(ui("命令"), command, code = true))
        val text = objectValue["text"].stringValue()
        if (objectValue["type"].stringValue() == "text" && text != null) {
            return listOf(ActivityPayloadBlock(ui("输入"), text))
        }
    }
    return listOf(ActivityPayloadBlock(ui("输入"), displayValue(value), code = value !is JsonPrimitive))
}

private fun outputBlocks(element: JsonElement): List<ActivityPayloadBlock> {
    val value = decoded(element)
    val result = unwrapResult(value)
    val resultObject = result as? JsonObject
    val blocks = buildList {
        if (resultObject != null) {
            resultObject.firstString("stdout_preview", "stdout", "output_preview")?.let {
                add(ActivityPayloadBlock(ui("标准输出"), it, code = true))
            }
            resultObject.firstString("stderr_preview", "stderr")?.let {
                add(ActivityPayloadBlock(ui("错误输出"), it, code = true))
            }
            resultObject.firstString("summary_text", "summary")?.let {
                add(ActivityPayloadBlock(ui("摘要"), it))
            }
            resultObject.firstString("error", "message")?.let {
                add(ActivityPayloadBlock(ui("错误"), it))
            }
            resultObject["exit_status"]?.jsonPrimitive?.longOrNull?.let {
                add(ActivityPayloadBlock(ui("退出状态"), it.toString()))
            }
        }
        if (isEmpty()) add(ActivityPayloadBlock(ui("输出"), displayValue(result), code = result !is JsonPrimitive))
    }
    return blocks
}

private fun unwrapResult(value: JsonElement): JsonElement {
    val objectValue = value as? JsonObject ?: return value
    val envelope = objectValue["envelope"] as? JsonObject
    val nested = envelope?.get("result") ?: objectValue["result"]
    return if (nested != null) unwrapResult(nested) else value
}

private fun decoded(value: JsonElement): JsonElement {
    if (value !is JsonPrimitive || !value.isString) return value
    return value.content.toJsonElementOrNull() ?: value
}

private fun displayValue(value: JsonElement): String {
    val decoded = decoded(value)
    return if (decoded is JsonPrimitive) {
        decoded.contentOrNull ?: decoded.toString()
    } else {
        prettyJson.encodeToString(JsonElement.serializer(), decoded)
    }
}

private fun readableObject(value: JsonObject): String =
    value.entries
        .filterNot { it.key in setOf("id", "event_seq", "revision") }
        .joinToString("\n") { (key, entry) -> "${labelFor(key)}: ${displayValue(entry)}" }
        .ifBlank { displayValue(value) }

private fun labelFor(key: String): String =
    key.replace('_', ' ').replace('-', ' ').replaceFirstChar { it.uppercase() }

private fun JsonObject.firstString(vararg keys: String): String? =
    keys.firstNotNullOfOrNull { key -> this[key].stringValue() }

private fun JsonElement?.stringValue(): String? =
    (this as? JsonPrimitive)?.contentOrNull

private fun String.toJsonElementOrNull(): JsonElement? =
    runCatching { prettyJson.parseToJsonElement(this) }.getOrNull()
