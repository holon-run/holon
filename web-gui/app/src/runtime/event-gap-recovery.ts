export interface SequencedEvent {
  event_seq?: number;
  event_log_epoch?: string;
}

export interface SequencedEventPage<T extends SequencedEvent> {
  events: T[];
  eventLogEpoch?: string;
}

interface AgentRecoveryState {
  generation: number;
  eventLogEpoch?: string;
  contiguousSeq: number;
  highestObservedSeq: number;
  observationVersion: number;
  backfillInFlight: boolean;
}

export interface AgentRecoverySnapshot {
  contiguousSeq: number;
  highestObservedSeq: number;
  recovering: boolean;
}

interface RecoveryCycle {
  generation: number;
  afterSeq: number;
  observationVersion: number;
}

export class EventGapRecoveryTracker {
  private readonly states = new Map<string, AgentRecoveryState>();
  private nextGeneration = 1;

  clear(): void {
    this.states.clear();
  }

  register(agentId: string, baselineSeq?: number, eventLogEpoch?: string): void {
    if (baselineSeq == null || this.states.has(agentId)) return;
    this.states.set(agentId, {
      generation: this.nextGeneration++,
      eventLogEpoch: normalizeEpoch(eventLogEpoch),
      contiguousSeq: baselineSeq,
      highestObservedSeq: baselineSeq,
      observationVersion: 0,
      backfillInFlight: false,
    });
  }

  unregister(agentId: string): void {
    this.states.delete(agentId);
  }

  observe(agentId: string, seq: number, eventLogEpoch?: string): AgentRecoverySnapshot {
    this.adoptEpoch(agentId, eventLogEpoch);
    let state = this.states.get(agentId);
    if (!state) {
      state = {
        generation: this.nextGeneration++,
        eventLogEpoch: normalizeEpoch(eventLogEpoch),
        contiguousSeq: seq,
        highestObservedSeq: seq,
        observationVersion: 0,
        backfillInFlight: false,
      };
      this.states.set(agentId, state);
      return this.snapshot(state);
    }

    state.observationVersion += 1;
    state.highestObservedSeq = Math.max(state.highestObservedSeq, seq);
    if (seq === state.contiguousSeq + 1) {
      state.contiguousSeq = seq;
    }
    return this.snapshot(state);
  }

  adoptEpoch(agentId: string, eventLogEpoch?: string): boolean {
    const incomingEpoch = normalizeEpoch(eventLogEpoch);
    const state = this.states.get(agentId);
    if (!incomingEpoch || !state) return false;
    if (!state.eventLogEpoch) {
      state.eventLogEpoch = incomingEpoch;
      return false;
    }
    if (state.eventLogEpoch === incomingEpoch) return false;
    this.states.set(agentId, {
      generation: this.nextGeneration++,
      eventLogEpoch: incomingEpoch,
      contiguousSeq: 0,
      highestObservedSeq: 0,
      observationVersion: 0,
      backfillInFlight: false,
    });
    return true;
  }

  snapshotFor(agentId: string): AgentRecoverySnapshot | undefined {
    const state = this.states.get(agentId);
    return state ? this.snapshot(state) : undefined;
  }

  beginBackfill(agentId: string, force: boolean): RecoveryCycle | undefined {
    const state = this.states.get(agentId);
    if (!state || state.backfillInFlight || (!force && state.highestObservedSeq <= state.contiguousSeq)) {
      return undefined;
    }
    state.backfillInFlight = true;
    return {
      generation: state.generation,
      afterSeq: state.contiguousSeq,
      observationVersion: state.observationVersion,
    };
  }

  acceptBackfill(
    agentId: string,
    cycle: RecoveryCycle,
    seqs: number[],
  ): AgentRecoverySnapshot | undefined {
    const state = this.states.get(agentId);
    if (!state || state.generation !== cycle.generation) return undefined;

    for (const seq of Array.from(new Set(seqs)).sort((left, right) => left - right)) {
      state.highestObservedSeq = Math.max(state.highestObservedSeq, seq);
      if (seq === state.contiguousSeq + 1) {
        state.contiguousSeq = seq;
      }
    }
    return this.snapshot(state);
  }

  nextCycle(agentId: string, previous: RecoveryCycle): RecoveryCycle | undefined {
    const state = this.states.get(agentId);
    if (
      !state ||
      state.generation !== previous.generation ||
      state.highestObservedSeq <= state.contiguousSeq ||
      (state.contiguousSeq <= previous.afterSeq && state.observationVersion === previous.observationVersion)
    ) {
      return undefined;
    }
    return {
      generation: state.generation,
      afterSeq: state.contiguousSeq,
      observationVersion: state.observationVersion,
    };
  }

  endBackfill(agentId: string, cycle: RecoveryCycle): AgentRecoverySnapshot | undefined {
    const state = this.states.get(agentId);
    if (!state || state.generation !== cycle.generation) return undefined;
    state.backfillInFlight = false;
    return this.snapshot(state);
  }

  private snapshot(state: AgentRecoveryState): AgentRecoverySnapshot {
    return {
      contiguousSeq: state.contiguousSeq,
      highestObservedSeq: state.highestObservedSeq,
      recovering: state.highestObservedSeq > state.contiguousSeq,
    };
  }
}

export async function recoverEventGap<T extends SequencedEvent>(
  tracker: EventGapRecoveryTracker,
  agentId: string,
  options: {
    force?: boolean;
    limit: number;
    fetchPage: (afterSeq: number) => Promise<SequencedEventPage<T>>;
    applyEvents: (events: T[]) => void;
  },
): Promise<void> {
  let cycle = tracker.beginBackfill(agentId, options.force ?? false);
  if (!cycle) return;
  let cleanupCycle = cycle;

  try {
    while (cycle) {
      let cursor = cycle.afterSeq;
      let hasMore = true;
      while (hasMore) {
        const page = await options.fetchPage(cursor);
        if (tracker.adoptEpoch(agentId, page.eventLogEpoch)) {
          const restartedCycle = tracker.beginBackfill(agentId, true);
          if (!restartedCycle) return;
          cycle = restartedCycle;
          cleanupCycle = restartedCycle;
          cursor = restartedCycle.afterSeq;
          continue;
        }
        const events = page.events.filter((event) => event.event_seq != null);
        if (!events.length) break;

        const snapshot = tracker.acceptBackfill(
          agentId,
          cycle,
          events.map((event) => event.event_seq as number),
        );
        if (!snapshot) return;
        options.applyEvents(events);
        const nextCursor = snapshot.contiguousSeq;
        hasMore = events.length >= options.limit && nextCursor > cursor;
        cursor = nextCursor;
      }
      cycle = tracker.nextCycle(agentId, cycle);
    }
  } finally {
    tracker.endBackfill(agentId, cleanupCycle);
  }
}

function normalizeEpoch(eventLogEpoch?: string): string | undefined {
  return eventLogEpoch || undefined;
}
