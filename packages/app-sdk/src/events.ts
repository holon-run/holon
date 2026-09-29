import type { AppEvent, AppEventsOptions } from "./types.js";

export function events(
  baseUrl: URL,
  options: AppEventsOptions = {},
): AsyncIterable<AppEvent> {
  return {
    [Symbol.asyncIterator](): AsyncIterator<AppEvent> {
      const queue: AppEvent[] = [];
      let wake: (() => void) | undefined;
      let closed = false;
      const source = new EventSource(
        new URL("events", baseUrl),
        { withCredentials: true },
      );
      const close = () => {
        if (closed) return;
        closed = true;
        source.close();
        wake?.();
      };
      source.addEventListener("holon_event", (event) => {
        queue.push(JSON.parse((event as MessageEvent).data) as AppEvent);
        wake?.();
        wake = undefined;
      });
      source.onerror = close;
      options.signal?.addEventListener("abort", close, { once: true });

      return {
        async next(): Promise<IteratorResult<AppEvent>> {
          while (!queue.length && !closed) {
            await new Promise<void>((resolve) => {
              wake = resolve;
            });
          }
          const value = queue.shift();
          if (value) return { done: false, value };
          return { done: true, value: undefined };
        },
        async return(): Promise<IteratorResult<AppEvent>> {
          close();
          return { done: true, value: undefined };
        },
      };
    },
  };
}
