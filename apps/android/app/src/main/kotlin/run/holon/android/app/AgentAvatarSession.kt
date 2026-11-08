package run.holon.android.app

/** The capability must come from this authenticated session, never a cached roster. */
internal fun activeAvatarLease(state: HolonUiState, lease: SessionLease?): SessionLease? =
    lease?.takeIf {
        state.phase == AppPhase.Ready && state.switchingNetworkId == null &&
            state.session?.scopeKey == it.session.scopeKey &&
            state.session?.server?.capabilities?.contains("agent_avatar_v1") == true
    }
