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
typed status (`loading` / `ready` / `reconnecting` / `unsupported` /
`recoverable_error` / `terminal_error`), and dispose semantics for scope
switches. `web-gui` wraps it through a reference-counted scope store
(`web-gui/app/src/runtime/conversation-scope-store.ts`) and the
`useConversationSession` hook; it is the only conversation-content source for
normal GUI pages. The legacy raw-event timeline, session catch-up, and
transcript hydration paths were removed from the GUI — see the cutover note in
`docs/rfcs/conversation-read-model.md` §9.1.
