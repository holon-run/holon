package run.holon.android.app

import run.holon.android.sdk.HolonConversationTurn

/** Expansion intent is independent of the selected detail resource and its loading state. */
internal fun preferredExpandedTurn(
    turns: List<HolonConversationTurn>,
    selected: HolonConversationTurn?,
    manuallyExpanded: String?,
    manuallyCollapsed: Set<String>,
    automaticallyExpanded: String?,
    readableBriefIds: Set<String>,
    readingDetails: Boolean,
): String? {
    if (manuallyExpanded != null) return manuallyExpanded
    if (selected != null) {
        // Deep-link and full-screen selections are explicit, not automatic choices.
        if (selected.id != automaticallyExpanded || readingDetails || !canAutoCollapseTurn(selected, readableBriefIds)) return selected.id
    }
    if (readingDetails) return null
    return turns.lastOrNull { it.isRunning() && it.id !in manuallyCollapsed }?.id
}

internal fun canAutoCollapseTurn(turn: HolonConversationTurn, readableBriefIds: Set<String>): Boolean =
    turn.settled && turn.executionKind == "terminal" && turn.terminalOutcome == "completed" &&
        turn.attentionKind == null && !turn.inputsTruncated &&
        (turn.resultKind == "none" || turn.resultKind == "available" &&
            turn.briefIds.isNotEmpty() && turn.briefIds.all { it in readableBriefIds })
