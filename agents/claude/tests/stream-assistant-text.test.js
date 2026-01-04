import { test, describe, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { ProgressLogger, AssistantOutputMode, LogLevel } from "../dist/agent.js";

describe("ProgressLogger - streamAssistantText Integration", () => {
    let originalConsoleLog;
    let logOutputs;

    // Helper to get only ASSISTANT messages
    function getAssistantOutputs() {
        return logOutputs.filter((log) => log.startsWith("[ASSISTANT]"));
    }

    beforeEach(() => {
        // Capture console.log output
        originalConsoleLog = console.log;
        logOutputs = [];
        console.log = (...args) => {
            logOutputs.push(args.map((arg) => String(arg)).join(" "));
        };
    });

    afterEach(() => {
        // Restore console.log
        console.log = originalConsoleLog;
    });
    describe("NONE mode prevents any output", () => {
        test("NONE mode does not output assistant text", () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.NONE);
            logger.streamAssistantText("This should not be output");
            assert.strictEqual(getAssistantOutputs().length, 0);
        });
        test("NONE mode ignores all text regardless of content", () => {
            const logger = new ProgressLogger(LogLevel.DEBUG, AssistantOutputMode.NONE);
            logger.streamAssistantText("Short text");
            logger.streamAssistantText("A".repeat(600));
            logger.streamAssistantText("");
            assert.strictEqual(getAssistantOutputs().length, 0);
        });
    });
    describe("STREAM mode outputs assistant text", () => {
        test("STREAM mode outputs text with [ASSISTANT] prefix", () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            logger.streamAssistantText("Hello, world!");
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.strictEqual(getAssistantOutputs()[0], "[ASSISTANT] Hello, world!");
        });
        test("STREAM mode trims whitespace before outputting", () => {
            const logger = new ProgressLogger(LogLevel.INFO, AssistantOutputMode.STREAM);
            logger.streamAssistantText("  Trimmed message  ");
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.strictEqual(getAssistantOutputs()[0], "[ASSISTANT] Trimmed message");
        });
        test("STREAM mode skips empty text", () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            logger.streamAssistantText("");
            assert.strictEqual(getAssistantOutputs().length, 0);
        });
        test("STREAM mode skips whitespace-only text", () => {
            const logger = new ProgressLogger(LogLevel.MINIMAL, AssistantOutputMode.STREAM);
            logger.streamAssistantText("   \n\t  ");
            assert.strictEqual(getAssistantOutputs().length, 0);
        });
    });
    describe("Rate limiting with streamAssistantText", () => {
        test("respects rate limiting (max 1 message per second)", () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            // First message should be output
            logger.streamAssistantText("First message");
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.strictEqual(getAssistantOutputs()[0], "[ASSISTANT] First message");
            // Immediate second message should be skipped
            logger.streamAssistantText("Second message");
            assert.strictEqual(getAssistantOutputs().length, 1); // Still only 1 output
        });
        test("allows output after rate limit period expires", async () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            // First message
            logger.streamAssistantText("First message");
            assert.strictEqual(getAssistantOutputs().length, 1);
            // Wait for rate limit to expire
            await new Promise((resolve) => setTimeout(resolve, 1100));
            // Second message should now be allowed
            logger.streamAssistantText("Second message");
            assert.strictEqual(getAssistantOutputs().length, 2);
            assert.strictEqual(getAssistantOutputs()[1], "[ASSISTANT] Second message");
        });
    });
    describe("Truncation with streamAssistantText", () => {
        test("truncates messages over 500 characters", () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            const longMessage = "A".repeat(600);
            logger.streamAssistantText(longMessage);
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.ok(getAssistantOutputs()[0].startsWith("[ASSISTANT] "));
            assert.ok(getAssistantOutputs()[0].endsWith("... (truncated)"));
        });
        test("does not truncate messages at exactly 500 characters", () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            const exactMessage = "B".repeat(500);
            logger.streamAssistantText(exactMessage);
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.strictEqual(getAssistantOutputs()[0], "[ASSISTANT] " + exactMessage);
            assert.ok(!getAssistantOutputs()[0].endsWith("... (truncated)"));
        });
        test("does not truncate short messages", () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            const shortMessage = "Short message";
            logger.streamAssistantText(shortMessage);
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.strictEqual(getAssistantOutputs()[0], "[ASSISTANT] Short message");
        });
    });
    describe("Total character cap with streamAssistantText", () => {
        test("stops outputting after hitting total character cap", async () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            // Send messages that will eventually hit the 10,000 character cap
            // Each message is 500 chars, so 20 messages = 10,000 chars
            for (let i = 0; i < 25; i++) {
                logger.streamAssistantText("M".repeat(500));
                // Wait for rate limit to allow next message
                await new Promise((resolve) => setTimeout(resolve, 1100));
            }
            // Should have stopped outputting after 20 messages
            // Note: The exact count may vary slightly due to timing, but it should be around 20
            assert.ok(getAssistantOutputs().length <= 21, `Expected <= 21 outputs, got ${getAssistantOutputs().length}`);
        });
        test("accurately counts characters toward total cap", async () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            // Send a message that will be truncated
            logger.streamAssistantText("Z".repeat(600));
            // The truncated content (500 chars) should be counted toward the cap
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.ok(getAssistantOutputs()[0].includes("... (truncated)"));
            // Wait for rate limit
            await new Promise((resolve) => setTimeout(resolve, 1100));
            // Send 19 more 500-char messages (total should be 500 + 19*500 = 10,000)
            for (let i = 0; i < 19; i++) {
                logger.streamAssistantText("A".repeat(500));
                await new Promise((resolve) => setTimeout(resolve, 1100));
            }
            // Should have output 20 messages total (1 truncated + 19 full)
            assert.strictEqual(getAssistantOutputs().length, 20);
            // Next message should be blocked by cap
            logger.streamAssistantText("B".repeat(500));
            await new Promise((resolve) => setTimeout(resolve, 1100));
            assert.strictEqual(getAssistantOutputs().length, 20);
        });
    });
    describe("Invalid assistantOutput mode handling", () => {
        test("defaults to NONE mode for invalid mode string", () => {
            // @ts-expect-error - Testing invalid mode
            const logger = new ProgressLogger(LogLevel.PROGRESS, "invalid_mode");
            logger.streamAssistantText("This should not be output");
            assert.strictEqual(getAssistantOutputs().length, 0);
        });
        test("handles undefined assistantOutput parameter", () => {
            // @ts-expect-error - Testing undefined parameter
            const logger = new ProgressLogger(LogLevel.PROGRESS, undefined);
            logger.streamAssistantText("This should not be output");
            assert.strictEqual(getAssistantOutputs().length, 0);
        });
    });
    describe("Combined behavior with rate limiting and truncation", () => {
        test("applies rate limiting and truncation together", async () => {
            const logger = new ProgressLogger(LogLevel.PROGRESS, AssistantOutputMode.STREAM);
            // First long message - should be truncated
            logger.streamAssistantText("A".repeat(600));
            assert.strictEqual(getAssistantOutputs().length, 1);
            assert.ok(getAssistantOutputs()[0].endsWith("... (truncated)"));
            // Immediate second message - should be rate limited
            logger.streamAssistantText("B".repeat(600));
            assert.strictEqual(getAssistantOutputs().length, 1);
            // Wait for rate limit
            await new Promise((resolve) => setTimeout(resolve, 1100));
            // Third message - should be allowed and truncated
            logger.streamAssistantText("C".repeat(600));
            assert.strictEqual(getAssistantOutputs().length, 2);
            assert.ok(getAssistantOutputs()[1].endsWith("... (truncated)"));
        });
    });
});
