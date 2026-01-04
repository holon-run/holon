import { test, describe } from "node:test";
import assert from "node:assert";
import { AssistantStreamLimiter } from "../dist/agent.js";
describe("AssistantStreamLimiter - Rate Limiting", () => {
    test("allows output when no previous output has occurred", () => {
        const limiter = new AssistantStreamLimiter();
        const result = limiter.shouldOutput("Hello, world!");
        assert.strictEqual(result, "Hello, world!");
    });
    test("rate limits: skips output when called within 1 second interval", () => {
        const limiter = new AssistantStreamLimiter();
        // First output should succeed
        const firstResult = limiter.shouldOutput("First message");
        assert.strictEqual(firstResult, "First message");
        // Immediate second output should be skipped
        const secondResult = limiter.shouldOutput("Second message");
        assert.strictEqual(secondResult, "");
    });
    test("rate limits: allows output after 1 second has elapsed", async () => {
        const limiter = new AssistantStreamLimiter();
        // First output should succeed
        const firstResult = limiter.shouldOutput("First message");
        assert.strictEqual(firstResult, "First message");
        // Wait for rate limit to expire
        await new Promise((resolve) => setTimeout(resolve, 1100));
        // Second output after delay should succeed
        const secondResult = limiter.shouldOutput("Second message");
        assert.strictEqual(secondResult, "Second message");
    });
    test("updates lastOutputTime after successful output", async () => {
        const limiter = new AssistantStreamLimiter();
        // First output
        limiter.shouldOutput("First");
        // Wait 500ms - should still be rate limited
        await new Promise((resolve) => setTimeout(resolve, 500));
        assert.strictEqual(limiter.shouldOutput("Second"), "");
        // Wait another 600ms - total 1100ms, should now be allowed
        await new Promise((resolve) => setTimeout(resolve, 600));
        assert.strictEqual(limiter.shouldOutput("Third"), "Third");
    });
});
describe("AssistantStreamLimiter - Truncation", () => {
    test("does not truncate short messages", () => {
        const limiter = new AssistantStreamLimiter();
        const shortMessage = "This is a short message";
        const result = limiter.shouldOutput(shortMessage);
        assert.strictEqual(result, shortMessage);
    });
    test("truncates messages exceeding 500 characters", () => {
        const limiter = new AssistantStreamLimiter();
        // Create a message that's 600 characters long
        const longMessage = "A".repeat(600);
        const result = limiter.shouldOutput(longMessage);
        // Should be truncated to 500 chars plus "... (truncated)" (three dots, space, parentheses)
        assert.strictEqual(result.length, 500 + "... (truncated)".length);
        assert.strictEqual(result, "A".repeat(500) + "... (truncated)");
    });
    test("does not truncate messages at exactly 500 characters", () => {
        const limiter = new AssistantStreamLimiter();
        const exactMessage = "B".repeat(500);
        const result = limiter.shouldOutput(exactMessage);
        // Should not be truncated since it's exactly at the limit
        assert.strictEqual(result, exactMessage);
        assert.strictEqual(result.length, 500);
    });
    test("adds truncation suffix for messages over 500 characters", () => {
        const limiter = new AssistantStreamLimiter();
        const overLimitMessage = "C".repeat(501);
        const result = limiter.shouldOutput(overLimitMessage);
        assert.strictEqual(result, "C".repeat(500) + "... (truncated)");
        assert(result.endsWith("... (truncated)"));
    });
});
describe("AssistantStreamLimiter - Total Character Cap", () => {
    test("stops outputting after hitting total character cap of 10,000", async () => {
        const limiter = new AssistantStreamLimiter();
        // Send 20 messages of 500 characters each with rate limiting delays
        // Wait 1.1s between each to avoid rate limiting
        for (let i = 0; i < 20; i++) {
            const message = "X".repeat(500);
            const result = limiter.shouldOutput(message);
            // These 20 messages should all succeed, bringing totalCharsSent to the 10,000-char cap
            assert.strictEqual(result, message);
            // Wait for rate limit to expire before next message
            await new Promise((resolve) => setTimeout(resolve, 1100));
        }
        // 21st message should hit the cap (totalCharsSent >= maxTotalChars) and return empty
        const message21 = "Y".repeat(500);
        const result21 = limiter.shouldOutput(message21);
        assert.strictEqual(result21, "");
    });
    test("accurately counts characters toward total cap", async () => {
        const limiter = new AssistantStreamLimiter();
        // Send message that would be truncated to 500 chars
        const longMessage = "Z".repeat(600);
        const result = limiter.shouldOutput(longMessage);
        assert.strictEqual(result, "Z".repeat(500) + "... (truncated)");
        // Total chars counted should be 500 (content length counted), not including truncation suffix
        // Wait for rate limit
        await new Promise((resolve) => setTimeout(resolve, 1100));
        // Send another 500 char message - should still be allowed
        const secondMessage = "A".repeat(500);
        const secondResult = limiter.shouldOutput(secondMessage);
        assert.strictEqual(secondResult, secondMessage);
    });
    test("prevents any output once cap is exceeded", async () => {
        const limiter = new AssistantStreamLimiter();
        // Send messages totaling 10,000 characters with rate limiting delays
        for (let i = 0; i < 20; i++) {
            limiter.shouldOutput("M".repeat(500));
            // Wait for rate limit before next message
            await new Promise((resolve) => setTimeout(resolve, 1100));
        }
        // Any further output should be blocked
        const result = limiter.shouldOutput("Should be blocked");
        assert.strictEqual(result, "");
    });
    test("handles partial messages that approach cap", async () => {
        const limiter = new AssistantStreamLimiter();
        // Send 19 messages of 500 chars = 9,500 total with rate limiting delays
        for (let i = 0; i < 19; i++) {
            limiter.shouldOutput("P".repeat(500));
            await new Promise((resolve) => setTimeout(resolve, 1100));
        }
        // Send a 600 char message - should be truncated to 500 and hit the cap
        const longMessage = "Q".repeat(600);
        const result = limiter.shouldOutput(longMessage);
        // Should succeed and bring total to 10,000
        assert.strictEqual(result, "Q".repeat(500) + "... (truncated)");
        // Any further output should be blocked
        const blocked = limiter.shouldOutput("Blocked");
        assert.strictEqual(blocked, "");
    });
});
describe("AssistantStreamLimiter - Empty Text Handling", () => {
    test("skips empty string", () => {
        const limiter = new AssistantStreamLimiter();
        const result = limiter.shouldOutput("");
        assert.strictEqual(result, "");
    });
    test("empty string does not update internal state", () => {
        const limiter = new AssistantStreamLimiter();
        // Empty string should be skipped
        const result1 = limiter.shouldOutput("");
        assert.strictEqual(result1, "");
        // Immediate call with valid text should succeed (proving empty didn't update lastOutputTime)
        const result2 = limiter.shouldOutput("Valid text");
        assert.strictEqual(result2, "Valid text");
    });
    test("skips whitespace-only text", () => {
        const limiter = new AssistantStreamLimiter();
        const result = limiter.shouldOutput("   \n\t  ");
        assert.strictEqual(result, "");
    });
    test("trims whitespace from valid text", () => {
        const limiter = new AssistantStreamLimiter();
        const result = limiter.shouldOutput("  Hello, world!  ");
        assert.strictEqual(result, "Hello, world!");
    });
    test("handles text with only newlines", () => {
        const limiter = new AssistantStreamLimiter();
        const result = limiter.shouldOutput("\n\n\n");
        assert.strictEqual(result, "");
    });
});
describe("AssistantStreamLimiter - Combined Behavior", () => {
    test("applies all rules: rate limiting, truncation, and total cap", async () => {
        const limiter = new AssistantStreamLimiter();
        // First message: should succeed and be truncated if needed
        const msg1 = "A".repeat(600);
        const result1 = limiter.shouldOutput(msg1);
        assert.strictEqual(result1, "A".repeat(500) + "... (truncated)");
        // Immediate second message: should be rate limited
        const result2 = limiter.shouldOutput("B".repeat(600));
        assert.strictEqual(result2, "");
        // Wait for rate limit to expire
        await new Promise((resolve) => setTimeout(resolve, 1100));
        // Third message: should succeed (rate limit expired)
        const result3 = limiter.shouldOutput("C".repeat(600));
        assert.strictEqual(result3, "C".repeat(500) + "... (truncated)");
    });
    test("preserves total cap state regardless of rate limiting", async () => {
        const limiter = new AssistantStreamLimiter();
        // Fill up to exactly at the cap with rate limiting delays
        for (let i = 0; i < 20; i++) {
            limiter.shouldOutput("F".repeat(500));
            await new Promise((resolve) => setTimeout(resolve, 1100));
        }
        // This should hit the cap (total >= 10,000)
        const result1 = limiter.shouldOutput("G".repeat(500));
        assert.strictEqual(result1, "");
        // Even after waiting for rate limit to expire, should still be blocked due to cap
        await new Promise((resolve) => setTimeout(resolve, 1100));
        const result2 = limiter.shouldOutput("H".repeat(500));
        assert.strictEqual(result2, "");
    });
    test("rate-limited messages do not count toward total cap", async () => {
        const limiter = new AssistantStreamLimiter();

        // First message: 500 chars (counts toward cap)
        const result1 = limiter.shouldOutput("A".repeat(500));
        assert.strictEqual(result1, "A".repeat(500));

        // Rapid second message: rate-limited, should NOT count toward cap
        const result2 = limiter.shouldOutput("B".repeat(500));
        assert.strictEqual(result2, "");

        // Wait for rate limit
        await new Promise((resolve) => setTimeout(resolve, 1100));

        // Third message: 500 chars (total should now be 1000, proving 2nd message didn't count)
        const result3 = limiter.shouldOutput("C".repeat(500));
        assert.strictEqual(result3, "C".repeat(500));

        // Send 16 more messages of 500 chars each (total should be 1000 + 16*500 = 9,000)
        for (let i = 0; i < 16; i++) {
            await new Promise((resolve) => setTimeout(resolve, 1100));
            limiter.shouldOutput("D".repeat(500));
        }

        // Wait for rate limit
        await new Promise((resolve) => setTimeout(resolve, 1100));

        // 19th message (1st + 3rd + 16 more = 18 successful so far): should bring total to 9,500
        const result4 = limiter.shouldOutput("E".repeat(500));
        assert.strictEqual(result4, "E".repeat(500));

        // Wait for rate limit
        await new Promise((resolve) => setTimeout(resolve, 1100));

        // 20th message: should bring total to exactly 10,000 and succeed
        const result5 = limiter.shouldOutput("F".repeat(500));
        assert.strictEqual(result5, "F".repeat(500));

        // Wait for rate limit
        await new Promise((resolve) => setTimeout(resolve, 1100));

        // 21st message: should be blocked because we've hit the cap (20 messages * 500 = 10,000)
        const result6 = limiter.shouldOutput("G".repeat(500));
        assert.strictEqual(result6, "");
    });
});
