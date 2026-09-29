interface HolonBrowserApi {
  readonly version: string;
  context(): Promise<unknown>;
  request(
    requestType: string,
    payload?: unknown,
    requestId?: string,
  ): Promise<unknown>;
  events(
    onEvent: (event: unknown) => void,
    onError?: (event: Event) => void,
  ): EventSource;
}

declare global {
  interface Window {
    Holon?: HolonBrowserApi;
  }
}

(() => {
  "use strict";
  const version = "1";
  const base = new URL(
    ".",
    document.currentScript instanceof HTMLScriptElement
      ? document.currentScript.src
      : location.href,
  );

  async function json(path: string, init: RequestInit = {}): Promise<unknown> {
    const headers = new Headers(init.headers);
    if (init.body !== undefined && !headers.has("content-type")) {
      headers.set("content-type", "application/json");
    }
    const response = await fetch(new URL(path, base), {
      credentials: "same-origin",
      ...init,
      headers,
    });
    const text = await response.text();
    let value: unknown = null;
    if (text) {
      try {
        value = JSON.parse(text) as unknown;
      } catch {
        value = text;
      }
    }
    if (!response.ok) {
      const message =
        typeof value === "object" &&
        value !== null &&
        "error" in value &&
        typeof value.error === "object" &&
        value.error !== null &&
        "message" in value.error &&
        typeof value.error.message === "string"
          ? value.error.message
          : "Holon App request failed";
      throw Object.assign(new Error(message), { status: response.status });
    }
    return value;
  }

  const holon = Object.freeze({
    version,
    context: () => json("context"),
    request: (requestType: string, payload?: unknown, requestId?: string) =>
      json("request", {
        method: "POST",
        body: JSON.stringify({
          version,
          request_type: requestType,
          payload: payload ?? null,
          ...(requestId === undefined ? {} : { request_id: requestId }),
        }),
      }),
    events: (onEvent: (event: unknown) => void, onError?: (event: Event) => void) => {
      const source = new EventSource(new URL("events", base), {
        withCredentials: true,
      });
      source.addEventListener("holon_event", (event) =>
        onEvent(JSON.parse((event as MessageEvent).data) as unknown),
      );
      source.onerror = onError ?? null;
      return source;
    },
  });

  window.Holon ??= holon;
})();
