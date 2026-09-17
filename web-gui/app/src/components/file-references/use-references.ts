import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { getRuntimeConnectionConfig, useRuntimeStore } from "../../runtime/runtime-store";
import type { ResolveFileReferenceResult } from "../../runtime/types";
import { ReferenceCache } from "./cache";
import type { MarkdownReference } from "./markdown";

const cache = new ReferenceCache();
function sourceIdentity(): string {
  const state = useRuntimeStore.getState();
  // Kept only in memory. Credentials never become a URL or persisted cache key.
  return JSON.stringify([getRuntimeConnectionConfig(), state.currentUser ?? null, state.currentUserLoaded]);
}
let previousIdentity = sourceIdentity();
let identityGeneration = 0;
let activeScope = JSON.stringify([previousIdentity, identityGeneration]);
function identity(): string {
  const next = sourceIdentity();
  if (next !== previousIdentity) {
    previousIdentity = next;
    activeScope = JSON.stringify([next, ++identityGeneration]);
    cache.setScope(activeScope);
  }
  return activeScope;
}
cache.setScope(activeScope);
useRuntimeStore.subscribe(identity);
export function useFileIdentity(): string {
  return useSyncExternalStore(useRuntimeStore.subscribe, identity, identity);
}

export function useReferences(references: Map<string, MarkdownReference>) {
  const scope = useFileIdentity();
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<{ scope: string; results: Map<string, ResolveFileReferenceResult> }>({ scope, results: new Map() });
  const retained = useRef({ scope, attempt, results: new Map<string, ResolveFileReferenceResult>() });
  useEffect(() => {
    let cancelled = false;
    const previous = retained.current.scope === scope && retained.current.attempt === attempt ? retained.current.results : new Map<string, ResolveFileReferenceResult>();
    const kept = new Map([...previous].filter(([key]) => references.has(key)));
    const items = [...references.values()].flatMap(({ key, value }) => value.kind === "file" && !kept.has(key) ? [{ key, reference: value.reference }] : []);
    void cache.resolve(scope, items, useRuntimeStore.getState().resolveFileReferences).then((results) => {
      if (!cancelled) {
        const merged = new Map([...kept, ...results]);
        retained.current = { scope, attempt, results: merged };
        setState({ scope, results: merged });
      }
    });
    return () => { cancelled = true; };
  }, [references, scope, attempt]);
  return { results: state.scope === scope ? state.results : new Map<string, ResolveFileReferenceResult>(), retry: () => setAttempt((value) => value + 1) };
}
