import assert from "node:assert/strict";
import test, { afterEach, beforeEach } from "node:test";

import { events } from "../dist/events.js";

class FakeEventSource {
  static instances = [];

  constructor(url, options) {
    this.url = String(url);
    this.options = options;
    this.closed = false;
    this.listeners = new Map();
    FakeEventSource.instances.push(this);
  }

  addEventListener(type, listener) {
    this.listeners.set(type, listener);
  }

  close() {
    this.closed = true;
  }

  emitEvent(data) {
    this.listeners.get("holon_event")?.({ data: JSON.stringify(data) });
  }

  emitError(event = new Event("error")) {
    this.onerror?.(event);
  }
}

const originalEventSource = globalThis.EventSource;
const originalDocument = globalThis.document;
const originalHtmlScriptElement = globalThis.HTMLScriptElement;
const originalLocation = globalThis.location;
const originalWindow = globalThis.window;

beforeEach(() => {
  globalThis.EventSource = FakeEventSource;
});

afterEach(() => {
  FakeEventSource.instances.length = 0;
});

afterEach(() => {
  globalThis.EventSource = originalEventSource;
  globalThis.document = originalDocument;
  globalThis.HTMLScriptElement = originalHtmlScriptElement;
  globalThis.location = originalLocation;
  globalThis.window = originalWindow;
});

test("async events yields queued events and uses the route-relative SSE endpoint", async () => {
  const iterator = events(new URL("https://example.test/apps/agent/app/"))[
    Symbol.asyncIterator
  ]();
  const source = FakeEventSource.instances[0];

  assert.equal(source.url, "https://example.test/apps/agent/app/events");
  assert.deepEqual(source.options, { withCredentials: true });

  const pending = iterator.next();
  source.emitEvent({
    version: "1",
    agent_id: "agent",
    app_id: "app",
    event: {
      sequence: 7,
      type: "message",
      timestamp: "2026-10-07T00:00:00Z",
      message_id: "message-7",
    },
  });

  assert.deepEqual(await pending, {
    done: false,
    value: {
      version: "1",
      agent_id: "agent",
      app_id: "app",
      event: {
        sequence: 7,
        type: "message",
        timestamp: "2026-10-07T00:00:00Z",
        message_id: "message-7",
      },
    },
  });
});

test("async events closes and ends on source error", async () => {
  const iterator = events(new URL("https://example.test/apps/agent/app/"))[
    Symbol.asyncIterator
  ]();
  const source = FakeEventSource.instances[0];
  const pending = iterator.next();

  source.emitError();

  assert.equal(source.closed, true);
  assert.deepEqual(await pending, { done: true, value: undefined });
  assert.deepEqual(await iterator.next(), { done: true, value: undefined });
});

test("async events closes on abort and iterator return", async (t) => {
  await t.test("abort", async () => {
    const controller = new AbortController();
    const iterator = events(new URL("https://example.test/apps/agent/app/"), {
      signal: controller.signal,
    })[Symbol.asyncIterator]();
    const source = FakeEventSource.instances[0];
    const pending = iterator.next();

    controller.abort();

    assert.equal(source.closed, true);
    assert.deepEqual(await pending, { done: true, value: undefined });
  });

  FakeEventSource.instances.length = 0;

  await t.test("return", async () => {
    const iterator = events(new URL("https://example.test/apps/agent/app/"))[
      Symbol.asyncIterator
    ]();
    const source = FakeEventSource.instances[0];

    assert.deepEqual(await iterator.return(), {
      done: true,
      value: undefined,
    });
    assert.equal(source.closed, true);
    assert.deepEqual(await iterator.next(), { done: true, value: undefined });
  });
});

test("browser events forwards parsed events and source errors", async () => {
  globalThis.document = { currentScript: null };
  globalThis.HTMLScriptElement = class {};
  globalThis.location = {
    href: "https://example.test/apps/agent/app/",
  };
  globalThis.window = {};

  await import("../dist/browser.js?contract-test");

  const received = [];
  const errors = [];
  const source = globalThis.window.Holon.events(
    (event) => received.push(event),
    (event) => errors.push(event),
  );

  assert.equal(source.url, "https://example.test/apps/agent/app/events");
  assert.deepEqual(source.options, { withCredentials: true });

  const event = {
    version: "1",
    agent_id: "agent",
    app_id: "app",
    event: {
      sequence: 8,
      type: "tool",
      timestamp: "2026-10-07T00:00:01Z",
      message_id: "message-8",
    },
  };
  source.emitEvent(event);
  const error = new Event("error");
  source.emitError(error);

  assert.deepEqual(received, [event]);
  assert.deepEqual(errors, [error]);
  assert.equal(source.closed, false);
});
