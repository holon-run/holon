package run.holon.android.app

import org.junit.Assert.assertEquals
import org.junit.Test

class UiCopyTest {
    @Test
    fun `network deletion copy identifies the local only boundary in both languages`() {
        assertEquals("Delete network", UiCopy.translate("删除网络", "en"))
        assertEquals("Delete this network?", UiCopy.translate("删除此网络？", "en"))
        assertEquals("Network deleted", UiCopy.translate("网络已删除", "en"))
        assertEquals("Deleting network…", UiCopy.translate("正在删除网络…", "en"))
        val warning = "此网络的本机配置、登录凭据、缓存、草稿、待发送消息和附件及诊断记录会被清除。远端主机和已发送的工作不受影响。"
        assertEquals(
            "This network’s saved settings, sign-in credentials, caches, drafts, unsent messages and attachments, and diagnostic logs will be removed from this device. The remote host and work already sent are not affected.",
            UiCopy.translate(warning, "en"),
        )
        assertEquals(warning, UiCopy.translate(warning, "zh-CN"))
        assertEquals(
            "This is the current network. Deleting it will disconnect and return to sign-in without connecting to another network automatically.",
            UiCopy.translate("这是当前网络。删除后将断开连接并返回登录页，不会自动连接其他网络。", "en"),
        )
    }

    @Test
    fun `background queue copy is localized without translating message content`() {
        assertEquals("Background messages", UiCopy.translate("后台消息", "en"))
        assertEquals("Tap a message for details", UiCopy.translate("点击消息查看详情", "en"))
        assertEquals("Queued", UiCopy.translate("排队中", "en"))
        assertEquals("Message details", UiCopy.translate("消息详情", "en"))
        assertEquals("Expand messages", UiCopy.translate("展开消息", "en"))
        assertEquals("Collapse messages", UiCopy.translate("收起消息", "en"))
        assertEquals("后台消息", UiCopy.translate("后台消息", "zh-CN"))
        assertEquals("child agent started: 请检查报告", UiCopy.translate("child agent started: 请检查报告", "en"))
    }

    @Test
    fun `privacy and session storage copy is translated accurately`() {
        assertEquals("Privacy policy", UiCopy.translate("隐私政策", "en"))
        assertEquals("隐私政策", UiCopy.translate("隐私政策", "zh-CN"))
        assertEquals(
            "The original access token is not written to disk; revocable session credentials are stored encrypted on this device.",
            UiCopy.translate("原始访问令牌不落盘；可撤销的会话凭据在设备上加密保存。", "en"),
        )
    }

    @Test
    fun `english uses web gui terminology`() {
        assertEquals("Results", UiCopy.translate("结果", "en"))
        assertEquals("Work items", UiCopy.translate("工作记录", "en"))
        assertEquals("Files", UiCopy.translate("文件", "en"))
        assertEquals("Current work", UiCopy.translate("当前工作", "en"))
        assertEquals("Tool calls", UiCopy.translate("工具调用", "en"))
        assertEquals("Needs attention", UiCopy.translate("需注意", "en"))
        assertEquals("Networks", UiCopy.translate("网络", "en"))
        assertEquals("Add network", UiCopy.translate("添加网络", "en"))
        assertEquals("Add and switch", UiCopy.translate("添加并切换", "en"))
        assertEquals("Saved networks", UiCopy.translate("已保存的网络", "en"))
    }

    @Test
    fun `model chooser copy is translated for english`() {
        assertEquals("Model", UiCopy.translate("模型", "en"))
        assertEquals("Select model", UiCopy.translate("选择模型", "en"))
        assertEquals("Active: ", UiCopy.translate("当前生效：", "en"))
        assertEquals("Agent override", UiCopy.translate("Agent 自定义", "en"))
        assertEquals("Auto · Runtime default", UiCopy.translate("Auto · 运行时默认", "en"))
        assertEquals("Auto · Reset to runtime default", UiCopy.translate("Auto · 恢复运行时默认", "en"))
        assertEquals("Refresh models", UiCopy.translate("刷新模型", "en"))
        assertEquals("Search models", UiCopy.translate("搜索模型", "en"))
        assertEquals("No model catalog yet. Refresh and try again.", UiCopy.translate("暂无模型目录，请刷新后重试", "en"))
        assertEquals("Apply to this Agent", UiCopy.translate("应用到此 Agent", "en"))
        assertEquals("Pending: ", UiCopy.translate("待应用：", "en"))
        assertEquals("Currently unavailable", UiCopy.translate("当前不可用", "en"))
        assertEquals("Default", UiCopy.translate("默认", "en"))
        assertEquals(
            "Model changes are saved to this Agent. Running tasks are not switched.",
            UiCopy.translate("模型更改保存到此 Agent；运行中的任务不会被切换。", "en"),
        )
        assertEquals("Model updated", UiCopy.translate("模型已更新", "en"))
        assertEquals("Restored the Auto model", UiCopy.translate("已恢复 Auto 模型", "en"))
        assertEquals("Pending", UiCopy.translate("待处理", "en"))
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
        assertEquals("1 unread result", UiCopy.translate("1 个未读结果", "en"))
        assertEquals("2 unread results", UiCopy.translate("2 个未读结果", "en"))
        assertEquals("Could not open file: missing", UiCopy.translate("文件无法打开：missing", "en"))
        assertEquals("Save failed: disk full. The selected location may contain an incomplete file.",
            UiCopy.translate("保存失败：disk full。所选位置可能留下不完整文件。", "en"))
    }
}
