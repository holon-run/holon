package run.holon.android.app

internal fun HolonUiState.withBriefReadSnapshot(snapshot: BriefReadSnapshot): HolonUiState =
    when (snapshot) {
        is BriefReadSnapshot.Server ->
            copy(
                briefReadStates = snapshot.states.mapValues { (id, incoming) ->
                    val cached = briefReadStates[id]
                    if (
                        cached != null &&
                            cached.eventLogEpoch == incoming.eventLogEpoch &&
                            cached.visibilityScopeId == incoming.visibilityScopeId &&
                            cached.revision > incoming.revision
                    ) {
                        cached
                    } else {
                        incoming
                    }
                },
                briefReadStatesLoaded = true,
                readBriefIds = emptyMap(),
                readBriefsLoaded = false,
                error = null,
                statusMessage = statusMessage.takeUnless { it == TRANSIENT_NETWORK_STATUS_MESSAGE },
            )
        is BriefReadSnapshot.Legacy ->
            copy(
                readBriefIds = snapshot.ids,
                readBriefsLoaded = true,
                briefReadStatesLoaded = false,
                error = null,
                statusMessage = statusMessage.takeUnless { it == TRANSIENT_NETWORK_STATUS_MESSAGE },
            )
    }

internal fun HolonUiState.withBriefReadFailure(error: Throwable): HolonUiState =
    if (error.isTransientNetworkFailure()) {
        copy(error = null, statusMessage = TRANSIENT_NETWORK_STATUS_MESSAGE)
    } else {
        val message = "无法加载未读数：${humanError(error)}"
        if (this.error == message) this else copy(error = message)
    }
