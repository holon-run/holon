import { disposeAllConversationScopes } from "./conversation-scope-store";
import { cacheClearAll } from "./idb-cache";

const AUTH_CHANGE_KEY = "holon.conversationAuthChange.v1";

/** Drop in-memory readers before removing persisted content on auth transitions. */
export async function clearConversationCaches(): Promise<void> {
  disposeAllConversationScopes(false);
  await cacheClearAll();
  try { localStorage.setItem(AUTH_CHANGE_KEY, crypto.randomUUID()); } catch { /* storage unavailable */ }
}

if (typeof window !== "undefined") {
  window.addEventListener("storage", (event) => {
    if (event.key !== AUTH_CHANGE_KEY) return;
    disposeAllConversationScopes(false);
    // A sibling tab changed the shared cookie; re-check authentication.
    window.location.reload();
  });
}
