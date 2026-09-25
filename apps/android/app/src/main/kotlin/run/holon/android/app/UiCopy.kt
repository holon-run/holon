package run.holon.android.app

import android.content.Context
import java.util.Locale

/**
 * Source-keyed copy keeps the existing Compose call sites small while this client gains i18n.
 * Only app-authored UI strings call [ui]; Agent messages, briefs, tool output and file contents do not.
 */
internal object UiCopy {
    private const val PREFS = "holon_ui_language"
    private const val LANGUAGE = "language"

    @Volatile private var systemLanguage = Locale.getDefault().language
    @Volatile private var preferredLanguage: String? = null

    fun initialize(context: Context) {
        systemLanguage = context.resources.configuration.locales[0].language
        preferredLanguage = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(LANGUAGE, null)
    }

    fun preference(): String? = preferredLanguage

    fun select(context: Context, language: String?) {
        require(language == null || language == "en" || language == "zh")
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().apply {
            if (language == null) remove(LANGUAGE) else putString(LANGUAGE, language)
        }.apply()
        preferredLanguage = language
    }

    fun text(source: String): String = translate(source, preferredLanguage ?: systemLanguage)

    internal fun translate(source: String, language: String): String {
        if (language.startsWith("zh", ignoreCase = true)) return chineseTerminology[source] ?: source
        return english[source] ?: dynamicEnglish(source) ?: source
    }

    private val chineseTerminology = mapOf(
        "Agents" to "智能体",
        "App" to "应用",
        "BLOCKED" to "受阻",
        "Runtime" to "运行时",
        "WORK ITEM" to "工作项",
        "工作记录" to "工作项",
        "工作详情" to "工作项详情",
        "还没有工作记录" to "还没有工作项",
        "当前筛选没有工作记录" to "当前筛选没有工作项",
        "加载更多工作记录" to "加载更多工作项",
        "返回工作列表" to "返回工作项",
        "查看工作详情" to "查看工作项",
    )

    private val english: Map<String, String> =
        """
        Agent 尚未连接可访问的工作区。|This Agent has no accessible workspace yet.
        Agent 的工作计划和验收结果会显示在这里。|The Agent's work plans and results will appear here.
        Assistant 文本|Assistant text
        HTTP 本身不加密；请只在可信局域网或 Tailscale 等加密隧道中使用|HTTP is not encrypted. Use it only on a trusted LAN or encrypted tunnel such as Tailscale.
        HTTPS 默认安全；HTTP 需要逐次确认。浏览器登录和扫码配对将在后续版本提供。|HTTPS is recommended. HTTP requires confirmation for each login. Browser login and QR pairing are coming later.
        Holon 地址|Holon address
        ‹ 会话|‹ Conversation
        ‹ 本轮|‹ Turn
        ‹ 返回文件列表|‹ Back to files
        ‹ 返回消息|‹ Back to message
        上次同步|Last synced
        下载|Download
        不包含地址、用户、session、token 或会话正文。|Excludes the address, user, session, token and conversation content.
        不支持的产物 locator|Unsupported artifact locator
        产物|Artifacts
        产物与关联工作|Artifacts and related work
        仅显示前 20,000 个字符；保存文件可查看完整内容|Showing only the first 20,000 characters. Save the file to see all content.
        从相册选择|Choose from photos
        代码|Code
        代码已复制|Code copied
        代码高亮|Syntax highlighting
        你的要求|Your request
        例如 https://holon.example.com 或 http://100.64.0.1:7878|For example, https://holon.example.com or http://100.64.0.1:7878
        保存|Save
        保存到设备|Save to device
        停止本轮|Stop turn
        全文可滚动查看|Scroll to read the full text
        全部|All
        关于|About
        关联工作|Related work
        关闭|Close
        关闭图片预览|Close image preview
        关闭搜索|Close search
        关闭预览|Close preview
        再按一次返回桌面|Press Back again to exit
        再次检查|Check again
        分享|Share
        分享或打开|Share or open
        分享结果|Share result
        分享脱敏诊断信息|Share redacted diagnostics
        分享连接诊断|Share connection diagnostics
        刚刚|Just now
        刷新|Refresh
        加载更多工作记录|Load more work items
        加载更早记录|Load earlier history
        加载更早过程|Load earlier activity
        协议|Protocol
        协议：holon-control/1|Protocol: holon-control/1
        原始记录|Raw record
        发送|Send
        发送中|Sending
        发送失败|Failed to send
        取消|Cancel
        受保护的工作区产物，可通过当前 session 读取|Protected workspace artifact, readable with the current session
        受阻|Blocked
        可继续|Ready to continue
        名称|Name
        图片|Image
        图片无法预览，可保存或分享后打开|Cannot preview this image. Save or share it to open elsewhere.
        地址|Address
        复制代码|Copy code
        失败|Failed
        完整计划|Full plan
        实时更新|Live updates
        尚无活动|No activity yet
        展开  ›|Expand  ›
        工作|Work
        工作中|In progress
        工作记录|Work items
        工作详情|Work item details
        工具调用|Tool calls
        已完成|Completed
        已排队|Queued
        已接收|Received
        已提交输入|Input submitted
        已连接|Connected
        已同步|Synced
        开始会话|Start a conversation
        当前工作|Current work
        当前步骤|Current step
        当前筛选没有工作记录|No work items match this filter
        当前身份|Current identity
        当前连接|Current connection
        待发送|Pending
        待运行|Ready to run
        我确认此地址位于可信网络或加密隧道中|I confirm this address is on a trusted network or encrypted tunnel
        打开|Open
        打开完整计划|Open full plan
        打开查看工具输入与输出|Open tool input and output
        执行中|Running
        执行记录|Activity
        技术详情|Technical details
        拍照|Take photo
        排版|Rendered
        搜索|Search
        搜索名称或 ID|Search name or ID
        摘要|Summary
        收起  ⌃|Collapse  ⌃
        收起原始记录|Hide raw record
        收起技术详情|Hide technical details
        收起步骤记录|Hide steps
        收起计划预览|Hide plan preview
        收起连接诊断|Hide connection diagnostics
        文件|Files
        文件为空|File is empty
        文件列表|File list
        文件较大，使用源码模式连续阅读|Large file: using source mode for continuous reading
        文件较大，已关闭高亮以保持滚动流畅|Large file: syntax highlighting disabled for smooth scrolling
        新结果|New result
        无法读取文本预览，请重新读取或保存后打开|Cannot read the text preview. Try again or save the file to open it.
        旧版会话无法完整关联到本轮|Older conversation activity cannot be fully linked to this turn
        显示|Show
        暂无新的工作摘要|No new work summary
        更新|Updated
        更早记录暂不可读取|Earlier history is unavailable
        更早过程暂不可读取|Earlier activity is unavailable
        最新|Newest
        最近产物|Recent artifacts
        有进行中的 WorkItem|Active work item
        本机移除|Remove locally
        本机缓存、草稿和待发送附件会被清除。已发送给主机的工作不会撤回。|Local cache, drafts and pending attachments will be cleared. Work already sent to the host will not be recalled.
        本轮仅保留了部分执行过程|Only part of this turn's activity is retained
        本轮失败|Turn failed
        本轮执行过程不可用|Turn activity unavailable
        本轮未完成|Turn incomplete
        本轮过程|Turn activity
        查找当前文件夹|Filter this folder
        查看 Agent 结果|View Agent results
        查看|View
        查看关联 brief|View related brief
        查看原始记录|View raw record
        查看工作详情|View work item
        查看计划预览|Preview plan
        查看过程  ›|View activity  ›
        查看连接诊断|View connection diagnostics
        正在准备附件…|Preparing attachments…
        正在恢复 Holon…|Restoring Holon…
        正在登录|Signing in
        正在等你回应|Waiting for your reply
        正在读取|Loading
        正在读取…|Loading…
        正在读取图片…|Loading image…
        正在读取文本预览…|Loading text preview…
        正在读取计划…|Loading plan…
        此 Holon 版本未提供计划文件的读取位置|This Holon version does not expose the plan file location
        此产物没有可读取 locator|This artifact has no readable locator
        此处只显示计划开头|Only the beginning of the plan is shown here
        步骤记录|Steps
        没有匹配的 Agent|No matching Agents
        没有匹配的文件|No matching files
        没有可浏览的 workspace|No browsable workspaces
        没有可读取的工具输入或输出。|No readable tool input or output.
        没有摘要|No summary
        没有目标说明|No objective provided
        没有结果摘要|No result summary
        消息|Messages
        添加附件|Add attachment
        清除|Clear
        版本|Version
        状态|Status
        用户|User
        登录|Sign in
        登录后只保存可撤销的会话，不保存原始令牌|Only a revocable session is saved after sign-in; the original token is not stored
        目前没有工作中的 Agent|No Agents in progress
        目前没有未读结果|No unread results
        目前没有需要回应的 Agent|No Agents need a reply
        目标|Objective
        目标、进度、结果与关联产物将在这里打开。|Objectives, progress, results and related artifacts will appear here.
        相关工作|Related work
        离线缓存|Offline cache
        空闲|Idle
        等任务结果|Waiting for task results
        等你回应|Waiting for your reply
        等外部变化|Waiting for external changes
        等待下一次调度|Waiting for the next run
        等待中|Waiting
        等待更多信息|Waiting for more information
        结果|Results
        结果、工作和文件将在这里打开。|Results, work items and files will open here.
        结果暂不可用|Result unavailable
        结果未知 · 将安全重试|Outcome unknown · will retry safely
        结果没有文本说明|Result has no text description
        结果载入中|Loading result
        给 Agent 发送消息…|Message the Agent…
        继续你正在进行的工作|Continue your work
        编辑|Edit
        能力|Capabilities
        自动换行|Wrap lines
        触发输入|Trigger input
        计划|Plan
        计划文件可用，但没有内联预览。|Plan file available, but there is no inline preview.
        计划预览|Plan preview
        认证|Authentication
        设置|Settings
        访问令牌不保存在设备上。|The access token is not stored on this device.
        访问令牌（token）|Access token
        调整搜索或显示隐藏文件。|Change the filter or hidden-file setting.
        调整搜索词或筛选条件后再试。|Change your search or filters and try again.
        较早的执行活动已超出保留窗口|Earlier activity is outside the retention window
        输入|Input
        输出|Output
        返回|Back
        返回上一级|Go to parent folder
        返回上一级继续浏览。|Go to the parent folder to continue browsing.
        返回工作列表|Back to work items
        返回结果|Back to results
        还没有 Agent|No Agents yet
        还没有工作记录|No work items yet
        这一段无法读取|Could not read this section
        这个文件夹是空的|This folder is empty
        这个筛选暂无 Agent|No Agents in this filter
        这项工作没有独立的结果摘要。|This work item has no separate result summary.
        这项工作的详情可直接打开，不依赖工作列表是否已加载。|Open this work item directly, even if the work list has not loaded.
        进度|Progress
        进行中|In progress
        连接 Holon|Connect to Holon
        连接已有 Holon 主机的移动工作台。打开应用后同步最新状态。|A mobile workspace for your Holon host. Opening the app syncs the latest state.
        连接成功后，Agent 会显示在这里。|Agents will appear here after you connect.
        连接诊断|Connection diagnostics
        退出并清除本机数据|Sign out and clear local data
        退出并清除本机数据？|Sign out and clear local data?
        退出登录|Sign out
        选择“全部”查看其他 Agent。|Select All to view other Agents.
        选择“全部”查看其他工作。|Select All to view other work items.
        选择“全部”查看所有 Agent。|Select All to view every Agent.
        选择一个 Agent|Select an Agent
        选择一个 WorkItem|Select a work item
        选择文件|Choose file
        部分执行活动暂不支持展示|Some activity cannot be displayed yet
        部分执行活动缺少本轮关联|Some activity is not linked to this turn
        重新登录|Sign in again
        重新登录当前主机|Sign in to this host again
        重新登录当前主机？|Sign in to this host again?
        重试|Retry
        链接|Link
        附件|Attachment
        隐藏|Hide
        隐藏文件|Hidden files
        需回应|Needs reply
        需注意|Needs attention
        需要一台已运行的 Holon 主机，以及该主机提供的访问令牌。|You need a running Holon host and its access token.
        需要处理|Needs attention
        预览|Preview
        预览产物|Preview artifact
        预览已截断|Preview truncated
        （空消息）|(Empty message)
        应用语言|App language
        系统默认|System default
        语言|Language
        HTTP 本身不加密，请确认仅在可信网络或加密隧道中使用|HTTP is not encrypted. Confirm that you are using a trusted network or encrypted tunnel.
        Holon 主机或登录身份已改变，请重新登录|The Holon host or signed-in identity changed. Sign in again.
        TLS 连接失败，请检查证书和 HTTPS 配置|TLS connection failed. Check the certificate and HTTPS configuration.
        daemon 不支持此功能，请检查版本|The daemon does not support this feature. Check its version.
        daemon 响应不兼容|Incompatible daemon response
        daemon 拒绝了兼容性握手|The daemon rejected the compatibility handshake
        daemon 未报告消息大小限制，请升级 daemon|The daemon did not report a message size limit. Upgrade the daemon.
        daemon 未报告附件大小限制，请升级 daemon|The daemon did not report attachment size limits. Upgrade the daemon.
        会话暂时离线，显示缓存|Conversation offline; showing cached content
        会话记录已重置，请刷新后重试|Conversation history was reset. Refresh and try again.
        只有未发送成功的消息可以移除|Only messages that failed to send can be removed
        只有未发送成功的消息可以编辑|Only messages that failed to send can be edited
        地址必须使用 HTTP 或 HTTPS|The address must use HTTP or HTTPS
        地址必须是完整的 Holon HTTP(S) 地址|Enter a complete Holon HTTP(S) address
        地址格式无效|Invalid address format
        地址路径只能为空或 /api|The address path must be empty or /api
        已从本机移除未发送消息|Unsent message removed from this device
        已打开文件；段落定位暂不可用|File opened; jumping to a section is not available yet
        已移回输入框；修改后发送会使用新的请求 ID|Moved back to the composer. Sending the edited message will use a new request ID.
        当前离线，显示上次同步内容|Offline; showing last synced content
        待发送附件已不存在，请重新选择文件|Pending attachment is missing. Choose the file again.
        执行记录已重置，请重新打开本轮过程|Activity was reset. Reopen this turn's activity.
        找不到 Holon 主机，请检查地址|Holon host not found. Check the address.
        无法写入所选位置|Cannot write to the selected location
        无法读取所选文件|Cannot read the selected file
        无法连接 Holon 主机，请确认 daemon 已启动|Cannot reach the Holon host. Make sure the daemon is running.
        无法连接 Holon，请检查网络和地址|Cannot connect to Holon. Check the network and address.
        服务端没有提供计划文件位置|The server did not provide the plan file location
        服务端没有提供计划的工作区标识|The server did not provide a workspace ID for the plan
        正在停止本轮…|Stopping turn…
        正在安全登录…|Signing in securely…
        消息不属于当前登录身份|This message belongs to a different signed-in identity
        登录已失效，请重新登录|Session expired. Sign in again.
        计划不属于当前 Agent|This plan belongs to a different Agent
        请求标识冲突，请重新发送|Request ID conflict. Send again.
        请输入 Holon 地址|Enter the Holon address
        请输入 token|Enter the token
        输入无效|Invalid input
        输入框已有内容，请先发送或清空，再编辑失败消息|The composer is not empty. Send or clear it before editing a failed message.
        连接中断，正在重连；当前显示上次同步的内容|Connection lost; reconnecting. Showing last synced content.
        附件或请求过大|Attachment or request is too large
        预览文件已不存在，请重新读取后再保存|The preview file is missing. Reload it before saving.
        文件已不可用|File is unavailable
        未知|Unknown
        """.trimIndent().lineSequence().filter { it.isNotBlank() }.associate { line ->
            val separator = line.indexOf('|')
            check(separator > 0) { "Invalid UI translation: $line" }
            line.substring(0, separator) to line.substring(separator + 1)
        }

    private fun dynamicEnglish(source: String): String? {
        fun match(pattern: String) = Regex(pattern).matchEntire(source)?.groupValues
        fun count(value: String, singular: String, plural: String = "${singular}s") =
            "$value ${if (value == "1") singular else plural}"

        match("(\\d+) 个 Agent · (已同步|已连接|离线缓存)(.*)")?.let { (_, value, status, clock) ->
            return "${count(value, "Agent")} · ${translate(status, "en")}$clock"
        }
        match("(\\d+) 条输入正在排队")?.let { return "${count(it[1], "input")} queued" }
        match("(\\d+) 条新活动")?.let { return "${count(it[1], "new activity", "new activities")}" }
        match("产生 (\\d+) 个产物")?.let { return "Produced ${count(it[1], "artifact")}" }
        match("已加载 (\\d+) 项 · (\\d+) 项进行中 · (\\d+) 项已完成")?.let {
            return "${it[1]} loaded · ${it[2]} in progress · ${it[3]} completed"
        }
        match("(\\d+)/(\\d+) 步")?.let { return "${it[1]}/${it[2]} steps" }
        match("查看步骤记录 · (\\d+)/(\\d+)")?.let { return "View steps · ${it[1]}/${it[2]}" }
        match("(\\d+) 分钟前")?.let { return "${count(it[1], "minute")} ago" }
        match("(\\d+) 小时前")?.let { return "${count(it[1], "hour")} ago" }
        match("(\\d+) 天前")?.let { return "${count(it[1], "day")} ago" }
        match("(\\d+) 周前")?.let { return "${count(it[1], "week")} ago" }
        match("附件限制：图片 (\\d+) B，文件 (\\d+) B")?.let { return "Attachment limits: image ${it[1]} B, file ${it[2]} B" }
        match("([^，]+)，点按放大")?.let { return "${it[1]}, tap to enlarge" }
        match("(.+) 不支持内置预览，可下载或分享后打开")?.let { return "No built-in preview for ${it[1]}. Download or share to open it." }
        match("(图片|文件)不能超过 (.+)")?.let { return "${translate(it[1], "en")} must not exceed ${it[2].replace(" 字节", " bytes")}" }
        match("消息和附件编码后不能超过 (.+)")?.let { return "The encoded message and attachments must not exceed ${it[1]}" }
        match("服务端错误（HTTP (\\d+)）")?.let { return "Server error (HTTP ${it[1]})" }
        match("协议不兼容：daemon (.+)，请升级 App 或 daemon")?.let { return "Incompatible protocol: daemon ${it[1]}. Upgrade the app or daemon." }

        val prefixes = listOf(
            "‹ 返回" to "‹ Back to ",
            "网页图片 · " to "Web image · ",
            "图片 · " to "Image · ",
            "状态：" to "Status: ",
            "认证方式：" to "Authentication: ",
            "能力：" to "Capabilities: ",
            "向 " to "Tell ",
            "过程可能不完整 · " to "Activity may be incomplete · ",
            "当前 · " to "Current · ",
            "移除 " to "Remove ",
            "分享 " to "Share ",
            "正在保存 " to "Saving ",
            "已保存到设备：" to "Saved to device: ",
            "无法保存已读状态：" to "Could not save read state: ",
            "列表同步已暂停：" to "List sync paused: ",
            "无法打开计划：" to "Could not open plan: ",
            "文件无法打开：" to "Could not open file: ",
            "不支持的文件类型：" to "Unsupported file type: ",
            "不支持的附件类型：" to "Unsupported attachment type: ",
            "待发送附件已不存在：" to "Pending attachment is missing: ",
            "daemon 缺少能力：" to "Daemon is missing capabilities: ",
            "无法读取文件：" to "Could not read file: ",
            "无法读取 Markdown：" to "Could not read Markdown: ",
        )
        prefixes.firstOrNull { source.startsWith(it.first) }?.let { (zh, en) ->
            val suffix = source.removePrefix(zh)
            if (zh == "向 " && suffix.endsWith(" 说明你希望完成的工作。")) {
                return "Tell ${suffix.removeSuffix(" 说明你希望完成的工作。")} what you want to get done."
            }
            val userSuppliedSuffixes = setOf("‹ 返回", "网页图片 · ", "图片 · ", "当前 · ", "移除 ", "分享 ", "正在保存 ")
            return en + if (zh in userSuppliedSuffixes) suffix else translate(suffix, "en")
        }
        if (source.startsWith("保存失败：") && source.endsWith("。所选位置可能留下不完整文件。")) {
            return "Save failed: ${translate(source.removePrefix("保存失败：").removeSuffix("。所选位置可能留下不完整文件。"), "en")}. The selected location may contain an incomplete file."
        }
        return null
    }
}

internal fun ui(source: String): String = UiCopy.text(source)
