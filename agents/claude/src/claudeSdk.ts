/**
 * Claude SDK Wrapper Module
 *
 * This module provides a transparent wrapper around the Claude Agent SDK
 * that can switch between real and mock implementations based on environment
 * variables.
 *
 * Environment Variables:
 * - HOLON_CLAUDE_DRIVER: Set to "mock" to use mock driver, otherwise uses real SDK
 * - HOLON_CLAUDE_MOCK_FIXTURE: Path to fixture file (required in mock mode)
 *
 * Usage:
 *   import { query } from "./claudeSdk.js";
 *
 *   // Real SDK (default)
 *   const stream = query({ prompt: "...", options: {...} });
 *
 *   // Mock driver
 *   HOLON_CLAUDE_DRIVER=mock HOLON_CLAUDE_MOCK_FIXTURE=/path/to/fixture.json
 *   const stream = query({ prompt: "...", options: {...} });
 */

// Re-export Options type from real SDK for 100% type compatibility
import type { Options, SDKSession, SDKSessionOptions } from "@anthropic-ai/claude-agent-sdk";

/**
 * Wrapper implementation of Claude SDK's query() function.
 *
 * Routes to either the real Claude Agent SDK or the mock driver based on
 * the HOLON_CLAUDE_DRIVER environment variable.
 *
 * @param params - Query parameters (prompt and options)
 * @returns AsyncIterable of SDK messages
 *
 * @example
 * // Real SDK (default)
 * for await (const msg of query({ prompt: "Fix the bug", options })) {
 *   console.log(msg);
 * }
 *
 * @example
 * // Mock driver
 * process.env.HOLON_CLAUDE_DRIVER = "mock";
 * process.env.HOLON_CLAUDE_MOCK_FIXTURE = "/path/to/fixture.json";
 * for await (const msg of query({ prompt: "Fix the bug", options })) {
 *   console.log(msg);
 * }
 */
export async function* query(
  params: { prompt: string; options?: Options }
): AsyncIterable<any> {
  const driver = process.env.HOLON_CLAUDE_DRIVER || "real";

  if (driver === "mock") {
    // Use mock driver for deterministic testing
    const { query: mockQuery } = await import("./mockDriver.js");
    yield* mockQuery(params);
  } else {
    // Use real Claude Agent SDK
    const { query: realQuery } = await import("@anthropic-ai/claude-agent-sdk");
    yield* realQuery(params);
  }
}

/**
 * Create a persistent Claude SDK session (V2 API).
 *
 * Note: mock driver currently does not emulate session API.
 */
export async function createSession(options: SDKSessionOptions): Promise<SDKSession> {
  const driver = process.env.HOLON_CLAUDE_DRIVER || "real";
  if (driver === "mock") {
    throw new Error("mock driver does not support persistent session API");
  }
  const { unstable_v2_createSession } = await import("@anthropic-ai/claude-agent-sdk");
  return unstable_v2_createSession(options);
}

/**
 * Resume a persistent Claude SDK session (V2 API).
 *
 * Note: mock driver currently does not emulate session API.
 */
export async function resumeSession(
  sessionId: string,
  options: SDKSessionOptions
): Promise<SDKSession> {
  const driver = process.env.HOLON_CLAUDE_DRIVER || "real";
  if (driver === "mock") {
    throw new Error("mock driver does not support persistent session API");
  }
  const { unstable_v2_resumeSession } = await import("@anthropic-ai/claude-agent-sdk");
  return unstable_v2_resumeSession(sessionId, options);
}

// Re-export Options type for external use
export type { Options };
