package run.holon.android.app

import org.junit.Assert.assertEquals
import org.junit.Test

class UiCopyTest {
    @Test
    fun `english uses web gui terminology`() {
        assertEquals("Results", UiCopy.translate("结果", "en"))
        assertEquals("Work items", UiCopy.translate("工作记录", "en"))
        assertEquals("Files", UiCopy.translate("文件", "en"))
        assertEquals("Current work", UiCopy.translate("当前工作", "en"))
        assertEquals("Tool calls", UiCopy.translate("工具调用", "en"))
        assertEquals("Needs attention", UiCopy.translate("需注意", "en"))
    }

    @Test
    fun `chinese copy and agent content stay unchanged`() {
        assertEquals("工作项", UiCopy.translate("工作记录", "zh"))
        assertEquals("工作项", UiCopy.translate("工作记录", "zh-CN"))
        assertEquals("智能体", UiCopy.translate("Agents", "zh"))
        assertEquals("## 用户写的 brief", UiCopy.translate("## 用户写的 brief", "en"))
        assertEquals("Share 结果", UiCopy.translate("分享 结果", "en"))
    }

    @Test
    fun `dynamic status and errors are translated without changing their values`() {
        assertEquals("3 Agents · Connected", UiCopy.translate("3 个 Agent · 已连接", "en"))
        assertEquals("2 minutes ago", UiCopy.translate("2 分钟前", "en"))
        assertEquals("Could not open file: missing", UiCopy.translate("文件无法打开：missing", "en"))
        assertEquals("Save failed: disk full. The selected location may contain an incomplete file.",
            UiCopy.translate("保存失败：disk full。所选位置可能留下不完整文件。", "en"))
    }
}
