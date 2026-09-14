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
const snapshot = await client.summary("main", { limit: 30 });
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
