import { describe, test } from "node:test";
import assert from "node:assert";
import { tryGetSessionId } from "../dist/sessionState.js";

describe("tryGetSessionId", () => {
  test("returns a trimmed session id", () => {
    const session = { sessionId: "  abc123  " };
    assert.strictEqual(tryGetSessionId(session), "abc123");
  });

  test("returns undefined for empty values", () => {
    const session = { sessionId: "   " };
    assert.strictEqual(tryGetSessionId(session), undefined);
  });

  test("handles thrown Error values and reports debug details", () => {
    const messages = [];
    const session = {
      get sessionId() {
        throw new Error("not ready");
      },
    };
    assert.strictEqual(tryGetSessionId(session, (message) => messages.push(message)), undefined);
    assert.strictEqual(messages.length, 1);
    assert.match(messages[0], /sessionId is not available yet/);
    assert.match(messages[0], /Error: not ready/);
  });

  test("handles non-Error thrown values", () => {
    const messages = [];
    const session = {
      get sessionId() {
        throw 404;
      },
    };
    assert.strictEqual(tryGetSessionId(session, (message) => messages.push(message)), undefined);
    assert.strictEqual(messages.length, 1);
    assert.match(messages[0], /404/);
  });
});
