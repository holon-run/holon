import { ConversationProtocolError, ConversationStaleResponseError } from "./errors.js";
import { ConversationProtocolState } from "./state.js";
import type {
  ConversationHistoryCursor,
  PendingInput,
  ConversationRequestIdentity,
  ConversationStateLimits,
  ConversationStreamBatch,
  ConversationSummaryResponse,
  ConversationTurnSummary,
} from "./types.js";

/** Internal adapter: server window cursors must never become tail cursors. */
export interface BrowsePage {
  readonly summary: ConversationSummaryResponse;
  readonly before: ConversationHistoryCursor | null;
  readonly after: ConversationHistoryCursor | null;
  readonly hasOlder: boolean;
  readonly hasNewer: boolean;
}

function compareTurns(a: ConversationTurnSummary, b: ConversationTurnSummary): number {
  return a.key.turn_index - b.key.turn_index || a.turn_id.localeCompare(b.turn_id);
}

/**
 * Bounded page membership plus the existing revision/detail reducer.
 * The complete stream replay is retained separately by the controller.
 */
export class ConversationBrowseWindow {
  readonly state: ConversationProtocolState;
  readonly snapshot: ConversationSummaryResponse;
  readonly #identity: ConversationRequestIdentity;
  #pages: BrowsePage[];
  #joined = false;
  #membershipHead: ConversationTurnSummary | undefined;

  constructor(
    identity: ConversationRequestIdentity,
    limits: ConversationStateLimits,
    page: BrowsePage,
  ) {
    this.#identity = identity;
    this.snapshot = page.summary;
    this.#pages = [page];
    this.state = new ConversationProtocolState(limits);
    this.state.bootstrap(identity, {
      ...page.summary,
      active_turns: [],
      next_before_cursor: null,
      has_more: false,
    });
    this.#membershipHead = [...page.summary.turns].sort(compareTurns).at(-1);
  }

  get before(): ConversationHistoryCursor | null { return this.#pages[0]!.before; }
  get after(): ConversationHistoryCursor | null { return this.#pages.at(-1)!.after; }
  get hasOlder(): boolean { return this.#pages[0]!.hasOlder; }
  get hasNewer(): boolean { return this.#pages.at(-1)!.hasNewer; }
  get joined(): boolean { return this.#joined; }

  append(page: BrowsePage, direction: "older" | "newer"): void {
    this.assertScope(page.summary);
    if (page.summary.turns.length === 0) {
      throw new ConversationProtocolError("browse page did not advance its membership");
    }
    const pages = direction === "older" ? [page, ...this.#pages] : [...this.#pages, page];
    // Evict whole far-end pages: the retained edge's server cursor can reload
    // every evicted turn; fabricating cursor boundaries would lose coverage.
    const count = () => new Set(pages.flatMap((entry) => entry.summary.turns.map((turn) => turn.turn_id))).size;
    while ((count() > this.state.limits.max_turns || pages.length > this.state.limits.max_turns) && pages.length > 1) {
      if (direction === "older") pages.pop();
      else pages.shift();
    }
    if (count() > this.state.limits.max_turns) {
      throw new ConversationProtocolError("browse page exceeds the configured turn bound");
    }
    this.state.replaceWindow(this.#identity, pages.flatMap((entry) => entry.summary.turns));
    this.#pages = pages;
    this.#joined = false;
    this.#membershipHead = [...pages.flatMap((entry) => entry.summary.turns)].sort(compareTurns).at(-1);
  }

  assertScope(page: ConversationSummaryResponse): void {
    for (const field of ["runtime_id", "event_log_epoch", "visibility_scope_id", "schema_version", "query_version"] as const) {
      if (page[field] !== this.snapshot[field]) {
        throw new ConversationStaleResponseError(`browse ${field} changed`);
      }
    }
    const expected = this.snapshot.coverage?.membership_upper_bound;
    const actual = page.coverage?.membership_upper_bound;
    if (expected !== undefined && (actual === undefined
      || expected?.turn_index !== actual?.turn_index
      || expected?.turn_id !== actual?.turn_id)) {
      throw new ConversationStaleResponseError("browse membership upper bound changed");
    }
  }

  applyBatch(batch: ConversationStreamBatch): void {
    const through = this.state.view().through_seq!;
    if (batch.begin.through_seq <= through) return;
    const loaded = new Set(this.state.view().turns.map((turn) => turn.turn_id));
    this.state.applyBatch(this.#identity, {
      ...batch,
      mutations: batch.mutations.filter((mutation) => {
        switch (mutation.type) {
          case "turn_summary_upsert": {
            const include = loaded.has(mutation.turn.turn_id) || (this.#joined
              && (this.#membershipHead === undefined || compareTurns(mutation.turn, this.#membershipHead) > 0));
            if (include) loaded.add(mutation.turn.turn_id);
            return include;
          }
          case "activity_upsert":
          case "detail_invalidated": return loaded.has(mutation.turn_id);
          case "operator_upsert":
          case "operator_remove": return this.#joined || this.state.view().pending_inputs.some(
            (input) => input.message_id === (mutation.type === "operator_upsert" ? mutation.input.message_id : mutation.message_id),
          );
        }
      }),
    });
  }

  /**
   * Extend fixed membership only after validating *all* post-snapshot batches.
   * IDs alone are not proof of continuous membership.
   */
  join(batches: readonly ConversationStreamBatch[], tailThrough: number, pendingInputs: readonly PendingInput[]): boolean {
    if (this.hasNewer) return false;
    const base = this.snapshot.snapshot_through_seq;
    if (tailThrough < base) return false;
    const replay = batches.filter((batch) => batch.begin.through_seq > base);
    let through = base;
    for (const batch of replay) {
      if (batch.begin.from_seq !== through) {
        throw new ConversationStaleResponseError("browse stream coverage expired; locate the source again");
      }
      through = batch.begin.through_seq;
    }
    if (through !== tailThrough) {
      throw new ConversationStaleResponseError("browse cannot bridge to the live tail without continuous stream coverage");
    }
    const current = this.state.view();
    const turns = current.turns;
    const head = [...turns].sort(compareTurns).at(-1);
    const additions = new Map<string, ConversationTurnSummary>();
    for (const batch of replay) {
      for (const mutation of batch.mutations) {
        if (mutation.type === "turn_summary_upsert"
          && !turns.some((turn) => turn.turn_id === mutation.turn.turn_id)
          && (head === undefined || compareTurns(mutation.turn, head) > 0)) {
          const existing = additions.get(mutation.turn.turn_id);
          if (!existing || mutation.turn.revision > existing.revision) additions.set(mutation.turn.turn_id, mutation.turn);
        }
      }
    }
    // Keep existing hydration/revisions while extending only proven new turns.
    let retained = turns;
    const pages = [...this.#pages];
    while (retained.length + additions.size > this.state.limits.max_turns && pages.length > 1) {
      pages.shift();
      const ids = new Set(pages.flatMap((page) => page.summary.turns.map((turn) => turn.turn_id)));
      retained = retained.filter((turn) => ids.has(turn.turn_id));
    }
    if (retained.length + additions.size > this.state.limits.max_turns) {
      throw new ConversationStaleResponseError("live bridge exceeds the bounded browse window; locate the source again");
    }
    this.state.replaceWindow(this.#identity, [...retained, ...additions.values()], pendingInputs);
    for (const batch of replay) this.state.refreshWindow(this.#identity, batch);
    this.#pages = pages;
    this.#joined = true;
    return true;
  }
}
