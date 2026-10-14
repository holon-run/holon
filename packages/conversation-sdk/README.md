# `@holon/conversation-sdk`

Independent browser/Node protocol client for Holon's bounded conversation read
model. It has no `web-gui` store, repository, or view-model dependency.

```ts
import {
  ConversationClient,
  ConversationProtocolState,
} from "@holon/conversation-sdk";

const client = new ConversationClient({
  baseUrl: "http://127.0.0.1:8787/api",
  bearerToken: () => sessionStorage.getItem("holon-token") ?? undefined,
});

await client.requireCapability();
const { summary: snapshot } = await client.summary("main", { limit: 30 });
const state = new ConversationProtocolState();
const identity = {
  remote_id: client.baseUrl,
  agent_id: "main",
  generation: 1,
};
state.bootstrap(identity, snapshot);

const checkpoint = state.reconnectCheckpoint();
const streamOptions = checkpoint === null ? {} : { after: checkpoint };
for await (const item of client.stream("main", streamOptions)) {
  if (item.type === "reset_required") {
    state.reset(item.reset.reason);
    break;
  }
  state.applyBatch(identity, item.batch);
}
```

`baseUrl` is the HTTP API root, normally ending in `/api`. `fetch`, headers,
and bearer-token resolution are injectable. The SSE transport uses streaming
`fetch`, so the same client supports authenticated browser and Node callers.

History/detail cursors and stream checkpoints are opaque branded strings. The
batch assembler exposes a stream batch only after the matching `checkpoint`
event and SSE `id` arrive. `ConversationProtocolState` applies that complete
batch atomically and bounds retained turns, live turns, pending inputs, detail
turns, and activities.

## Compatibility

The v1 client fails closed unless `/handshake` reports both:

- control protocol `holon-control` version `1`;
- capability `agents.conversation-read.v1`.

Capability absence means the conversation surface is unavailable; callers
should not probe the conversation routes. Summary, activity, and stream
boundaries must report conversation schema/query version `1`. Unknown control,
schema, or query versions require a client/server upgrade rather than
best-effort decoding.

The package supports browser and Node streaming `fetch`; its declared Node
engine is Node 24 or newer.

## Controller and GUI integration

The framework-agnostic `ConversationController` (added with the Web GUI
cutover) owns the page lifecycle around the client and protocol state:
serialized snapshot bootstrap, bounded-backoff stream reconnection with
serialized reset re-snapshotting, single-flight history/detail/brief requests,
typed status (`loading` / `paused` / `ready` / `reconnecting` / `unsupported` /
`recoverable_error` / `terminal_error`), and dispose semantics for scope
switches. `web-gui` wraps it through a reference-counted scope store
(`web-gui/app/src/runtime/conversation-scope-store.ts`) and the
`useConversationSession` hook; it is the only conversation-content source for
normal GUI pages. The legacy raw-event timeline, session catch-up, and
transcript hydration paths were removed from the GUI — see the cutover note in
`docs/rfcs/conversation-read-model.md` §9.1.

`pause()` releases the live stream while retaining the in-memory checkpoint;
`start()` resumes it without another summary unless the server asks for a reset.
`dispose()` flushes pending snapshot writes; use `dispose(false)` when discarding
credentials to cancel queued writes instead.

Optional brief and snapshot caches support `clear()` for access revocation.
Cache reads have a 600 ms budget before network fallback. Snapshot changes
are persisted every 750 ms while dirty, with immediate flush on terminal turns,
pause, and disposal. Locally changed snapshots discard their old ETag and
must revalidate before streaming. A 401/403 (or a missing conversation) clears
both visible state and persistent cache; a missing individual brief does not
discard the conversation. Hosts must namespace persistent adapters by verified
login identity and clear them on authentication changes.

### Canonical source navigation

After the controller is ready, `await controller.locateSource(sourceRef)`
resolves a canonical source through the additive
`agents.conversation-locate.v1` capability. `browseState()` exposes the normalized
typed target, `resolving` / `ready` / `source_only` / `error` status, independent
`older` / `newer` load states, `hasOlder`, `hasNewer`, `hasLiveUpdates`, and
`liveConnected`. Source-only results are not successful positioning.

`view()` retains the canonical live tail. `displayView()` selects the independent
bounded browsing window when present, or the live tail otherwise.
`loadOlderHistory()` pages the current mode;
`loadNewerHistory()` extends browsing toward the fixed snapshot's newest turn.
Whole far-end pages are evicted at the configured turn bound, retaining real
server cursors for reloading them. Initial positioning belongs to the host;
after the user scrolls, preserve the current reading anchor rather than pinning
the original target indefinitely. Existing detail/brief methods hydrate the
selected view without raw-event or transcript fallback.

The live tail keeps consuming its own stream, but new distant turns are not
inserted into browsing. Reaching the fixed newest boundary joins live membership
only with continuous post-locate batch coverage. At most 256 complete batches
are retained for that bridge; expired coverage fails recoverably and requires
an explicit re-locate. Epoch/scope resets invalidate navigation, and navigation
generations fence responses even if a transport ignores abort.

`view().checkpoint` and `through_seq` always come from the retained live tail,
never from a historical snapshot. `browsingView()` carries no read authority.
`displayView()` also carries no read authority while browsing. Hosts must gate
read marking during navigation and browsing; `browseState().liveConnected`
only reports data continuity, not read eligibility or scroll intent. Restoring
read eligibility requires the host's physical latest-viewport policy.
`returnToLatest()` explicitly leaves browsing. Hosts must also suppress automatic
scroll-to-latest during browsing and resolving/source-only/error states.
