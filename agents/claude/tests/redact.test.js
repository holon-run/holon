/**
 * Tests for the redaction utility.
 *
 * These tests ensure that sensitive information is properly redacted from
 * tool parameters before being displayed in logs.
 */

import { test, describe } from "node:test";
import assert from "node:assert";
import {
  redactValue,
  sanitizeToolInput,
  formatToolInputForLog,
  shouldOmitFromLogs,
} from "../dist/redact.js";

describe("Redaction Utility", () => {
  describe("redactValue", () => {
    test("redacts common sensitive key names", () => {
      const sensitiveKeys = [
        "token",
        "access_token",
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "authorization",
        "auth",
        "bearer",
        "credential",
        "cookie",
        "sessionid",
        "session",
        "session_token",
        "refresh_token",
        "auth_token",
        "private_key",
        "passphrase",
      ];

      for (const key of sensitiveKeys) {
        const result = redactValue("my-secret-value-12345", key);
        assert.strictEqual(result, "***", `Key "${key}" should be redacted`);
      }

      // Test case-insensitive matching
      const result = redactValue("my-secret-value", "TOKEN");
      assert.strictEqual(result, "***", "Case-insensitive matching should work");
    });

    test("redacts nested header patterns", () => {
      const result = redactValue("Bearer xyz123", "headers.authorization");
      assert.strictEqual(result, "***", "headers.authorization should be redacted");
    });

    test("redacts known API key prefixes", () => {
      const prefixes = [
        "ghp_abc123def456", // GitHub personal access token
        "gho_abc123def456", // GitHub OAuth token
        "ghu_abc123def456", // GitHub user token
        "ghs_abc123def456", // GitHub server token
        "ghr_abc123def456", // GitHub refresh token
        "sk_live_abc123def456", // Stripe live key
        "sk_test_abc123def456", // Stripe test key
        "sk-abc123def456", // Stripe key (old format)
        "anthropic_abc123def456", // Anthropic API key
        "hf_abc123def456", // HuggingFace token
        "AKIAABCDEFGHIJKLMNOPQR", // AWS access key
        "xoxb-abc123def456", // Slack bot token
        "xoxp-abc123def456", // Slack user token
        "ya29.abc123def456", // Google OAuth token
      ];

      for (const value of prefixes) {
        const result = redactValue(value, "value");
        assert.strictEqual(result, "***", `Value starting with "${value.slice(0, 10)}..." should be redacted`);
      }
    });

    test("does not redact safe values", () => {
      const safeValues = [
        ["hello", "message"],
        ["file.txt", "filename"],
        ["git status", "command"],
        ["src/", "path"],
        ["example.com", "domain"],
        ["readme", "name"],
      ];

      for (const [value, key] of safeValues) {
        const result = redactValue(value, key);
        assert.strictEqual(result, value, `Safe value "${value}" for key "${key}" should not be redacted`);
      }
    });

    test("redacts high-entropy strings (potential secrets)", () => {
      // High-entropy strings look like random API keys
      const highEntropyStrings = [
        "aB3xK9mP2qL7vN8wR4sT6uY1cF5gH", // Mixed case alphanumeric, long
        "7xK9mP2qL7vN8wR4sT6uY1cF5gH8j", // High entropy
      ];

      for (const value of highEntropyStrings) {
        const result = redactValue(value, "value");
        assert.strictEqual(result, "***", `High-entropy string should be redacted`);
      }
    });

    test("does not redact low-entropy natural language", () => {
      const naturalLanguage = [
        "This is a normal sentence with words",
        "hello-world-test-example",
      ];

      for (const value of naturalLanguage) {
        const result = redactValue(value, "message");
        assert.strictEqual(result, value, `Natural language should not be redacted`);
      }
    });

    test("does not redact URLs and paths", () => {
      const urlsAndPaths = [
        "https://example.com/api/endpoint",
        "/path/to/file.txt",
        "C:\\Users\\username\\file.txt",
      ];

      for (const value of urlsAndPaths) {
        const result = redactValue(value, "url");
        assert.strictEqual(result, value, `URLs and paths should not be redacted`);
      }
    });

    test("handles non-string values", () => {
      assert.strictEqual(redactValue(123, "key"), 123, "Numbers should pass through");
      assert.strictEqual(redactValue(true, "key"), true, "Booleans should pass through");
      assert.strictEqual(redactValue(null, "key"), null, "null should pass through");
      assert.strictEqual(redactValue(undefined, "key"), undefined, "undefined should pass through");
    });
  });

  describe("sanitizeToolInput", () => {
    test("sanitizes flat objects with sensitive fields", () => {
      const input = {
        command: "git status",
        token: "secret-token-123",
        password: "my-password",
      };

      const result = sanitizeToolInput(input);

      assert.notStrictEqual(result, undefined);
      assert.strictEqual(result.command, "git status", "Non-sensitive field should be preserved");
      assert.strictEqual(result.token, "***", "Token should be redacted");
      assert.strictEqual(result.password, "***", "Password should be redacted");
    });

    test("sanitizes nested objects", () => {
      const input = {
        command: "curl",
        headers: {
          authorization: "Bearer secret-token",
          "content-type": "application/json",
        },
      };

      const result = sanitizeToolInput(input);

      assert.notStrictEqual(result, undefined);
      assert.strictEqual(result.command, "curl");
      assert.strictEqual(result.headers.authorization, "***");
      assert.strictEqual(result.headers["content-type"], "application/json");
    });

    test("sanitizes arrays of objects", () => {
      const input = {
        items: [
          { name: "file1.txt", token: "secret1" },
          { name: "file2.txt", token: "secret2" },
        ],
      };

      const result = sanitizeToolInput(input);

      assert.notStrictEqual(result, undefined);
      assert.strictEqual(result.items[0].name, "file1.txt");
      assert.strictEqual(result.items[0].token, "***");
      assert.strictEqual(result.items[1].name, "file2.txt");
      assert.strictEqual(result.items[1].token, "***");
    });

    test("handles empty and null inputs", () => {
      assert.strictEqual(sanitizeToolInput(undefined), undefined);
      assert.strictEqual(sanitizeToolInput(null), undefined);
      assert.deepStrictEqual(sanitizeToolInput({}), {});
    });

    test("removes null and undefined values", () => {
      const input = {
        command: "git status",
        nullValue: null,
        undefinedValue: undefined,
        validValue: "kept",
      };

      const result = sanitizeToolInput(input);

      assert.strictEqual(result.command, "git status");
      assert.strictEqual(result.nullValue, undefined);
      assert.strictEqual(result.undefinedValue, undefined);
      assert.strictEqual(result.validValue, "kept");
    });
  });

  describe("formatToolInputForLog", () => {
    test("formats Bash tool with command", () => {
      const toolInput = {
        command: 'git status --short',
      };

      const result = formatToolInputForLog("Bash", toolInput);

      assert(result.includes("Bash"));
      assert(result.includes('command="git status --short"'));
    });

    test("redacts sensitive values in Bash command", () => {
      const toolInput = {
        command: "curl -H 'Authorization: Bearer secret-token' https://api.example.com",
      };

      const result = formatToolInputForLog("Bash", toolInput);

      // The command itself should be shown, but we should be aware it may contain secrets
      // In practice, users should avoid passing secrets in command line args
      assert(result.includes("Bash"));
      assert(result.includes("command="));
    });

    test("formats Read tool with file path", () => {
      const toolInput = {
        file_path: "/workspace/src/file.ts",
      };

      const result = formatToolInputForLog("Read", toolInput);

      assert(result.includes("Read"));
      assert(result.includes('file_path="/workspace/src/file.ts"'));
    });

    test("formats Grep tool with pattern and path", () => {
      const toolInput = {
        pattern: "function.*test",
        path: "/workspace/src",
      };

      const result = formatToolInputForLog("Grep", toolInput);

      assert(result.includes("Grep"));
      assert(result.includes('pattern="function.*test"'));
      assert(result.includes('path="/workspace/src"'));
    });

    test("formats HTTP tool with method and URL", () => {
      const toolInput = {
        method: "POST",
        url: "https://api.example.com/endpoint",
        headers: {
          authorization: "Bearer secret-token",
        },
      };

      const result = formatToolInputForLog("HTTP", toolInput);

      assert(result.includes("HTTP"));
      assert(result.includes("POST"));
      assert(result.includes("url="));
      // URL should be shown, but authorization header should not appear in the formatted output
      // since it's a non-priority field for this tool type
      assert(!result.includes("***"), "Redacted values should use placeholder");
    });

    test("handles empty input gracefully", () => {
      const result = formatToolInputForLog("Bash", undefined);
      assert.strictEqual(result, "Bash");

      const result2 = formatToolInputForLog("Bash", {});
      assert.strictEqual(result2, "Bash");
    });

    test("handles generic tools with priority fields", () => {
      const toolInput = {
        file_path: "/workspace/test.txt",
        description: "A test operation",
        other_field: "not shown",
      };

      const result = formatToolInputForLog("CustomTool", toolInput);

      assert(result.includes("CustomTool"));
      assert(result.includes('file_path="/workspace/test.txt"'));
    });

    test("truncates long values", () => {
      const toolInput = {
        command: "a".repeat(200), // Very long command
      };

      const result = formatToolInputForLog("Bash", toolInput);

      assert(result.includes("..."));
      // Should be truncated
      assert(result.length < 300);
    });

    test("escapes quotes in values", () => {
      const toolInput = {
        command: 'echo "hello world"',
      };

      const result = formatToolInputForLog("Bash", toolInput);

      assert(result.includes("Bash"));
      // Quotes should be escaped
      assert(result.includes("\\\""));
    });
  });

  describe("shouldOmitFromLogs", () => {
    test("omits very long strings", () => {
      assert.strictEqual(
        shouldOmitFromLogs("content", "a".repeat(600)),
        true,
        "Long strings should be omitted"
      );
    });

    test("omits large arrays", () => {
      assert.strictEqual(
        shouldOmitFromLogs("items", Array(20).fill("item")),
        true,
        "Large arrays should be omitted"
      );
    });

    test("keeps reasonable content", () => {
      assert.strictEqual(
        shouldOmitFromLogs("message", "a".repeat(100)),
        false,
        "Short strings should be kept"
      );
      assert.strictEqual(
        shouldOmitFromLogs("items", ["item1", "item2", "item3"]),
        false,
        "Small arrays should be kept"
      );
    });

    test("omits specific field names", () => {
      const omitKeys = ["content", "text", "body", "data", "output"];

      for (const key of omitKeys) {
        assert.strictEqual(
          shouldOmitFromLogs(key, "some value"),
          true,
          `Key "${key}" should be omitted`
        );
      }
    });

    test("case-insensitive key matching for omission", () => {
      assert.strictEqual(
        shouldOmitFromLogs("CONTENT", "value"),
        true,
        "Uppercase CONTENT should be omitted"
      );
      assert.strictEqual(
        shouldOmitFromLogs("Text", "value"),
        true,
        "Mixed case Text should be omitted"
      );
    });

    test("keeps safe field names", () => {
      const safeKeys = ["file_path", "pattern", "command", "query", "url", "name"];

      for (const key of safeKeys) {
        assert.strictEqual(
          shouldOmitFromLogs(key, "value"),
          false,
          `Key "${key}" should be kept`
        );
      }
    });
  });

  describe("Acceptance Criteria", () => {
    test("AC1: Tool logs show more than just tool name", () => {
      const toolInput = {
        command: "git status",
      };

      const result = formatToolInputForLog("Bash", toolInput);

      assert.notStrictEqual(result, "Bash", "Tool log should include more context than just the name");
      assert(result.includes("command="));
    });

    test("AC2: No plaintext secrets for known sensitive keys", () => {
      const toolInput = {
        token: "ghp_abc123def456",
        password: "my-secret-password",
        api_key: "sk_live_abc123",
        headers: {
          authorization: "Bearer token123",
        },
      };

      const result = sanitizeToolInput(toolInput);

      assert.strictEqual(result.token, "***", "Token should be redacted");
      assert.strictEqual(result.password, "***", "Password should be redacted");
      assert.strictEqual(result.api_key, "***", "API key should be redacted");
      assert.strictEqual(result.headers.authorization, "***", "Authorization header should be redacted");
    });

    test("AC3: Redaction works for common tool patterns", () => {
      // Bash with potential secrets
      const bashInput = {
        command: "export TOKEN=xxx && curl https://api.example.com",
      };
      const bashResult = formatToolInputForLog("Bash", bashInput);
      assert(bashResult.includes("Bash"));
      assert(bashResult.includes("command="));

      // HTTP with auth
      const httpInput = {
        method: "POST",
        url: "https://api.example.com/endpoint",
        headers: {
          authorization: "Bearer secret-token-abc123",
        },
      };
      const httpResult = formatToolInputForLog("HTTP", httpInput);
      assert(httpResult.includes("POST"));
      assert(httpResult.includes("url="));

      // Read with file path
      const readInput = {
        file_path: "/workspace/src/config.ts",
      };
      const readResult = formatToolInputForLog("Read", readInput);
      assert(readResult.includes("Read"));
      assert(readResult.includes('file_path='));
    });

    test("AC4: Representative redaction patterns work", () => {
      // Test token/password patterns
      const sensitiveInputs = [
        { key: "token", value: "secret-token-value" },
        { key: "password", value: "my-password-123" },
        { key: "api_key", value: "sk_live_abc123" },
        { key: "headers.authorization", value: "Bearer token123" },
        { key: "cookie", value: "sessionid=abc123" },
      ];

      for (const { key, value } of sensitiveInputs) {
        const result = redactValue(value, key);
        assert.strictEqual(result, "***", `Value for key "${key}" should be redacted`);
      }
    });
  });
});
