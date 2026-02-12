/**
 * Shared TypeScript interfaces for the mock driver.
 *
 * These interfaces define the fixture file format used by the mock driver
 * to simulate Claude SDK behavior for deterministic E2E testing.
 */

/**
 * MockFixture defines the structure of a mock driver fixture file.
 *
 * Fixtures are JSON files that describe deterministic operations to perform
 * in the workspace and the expected outcome.
 */
export interface MockFixture {
  /** Schema version for compatibility checking */
  version: "v1";
  /** Human-readable description of the test scenario */
  description?: string;
  /** Array of file operations to execute deterministically */
  operations: MockOperation[];
  /** Expected outcome of the mock execution */
  outcome: MockOutcome;
}

/**
 * MockOperation represents a single file system operation to execute.
 */
export interface MockOperation {
  /** Type of operation to perform */
  type: "write_file" | "append_file" | "delete_file" | "write_output";
  /**
   * Path relative to workspace root for file operations,
   * or relative to HOLON_OUTPUT_DIR (default /holon/output) for write_output operations.
   */
  path: string;
  /** Content to write or append (not used for delete_file) */
  content?: string;
}

/**
 * MockOutcome defines the expected result of the mock execution.
 */
export interface MockOutcome {
  /** Whether the mock execution should succeed */
  success: boolean;
  /** Text to emit in the result message */
  result_text: string;
  /** Optional summary.md content (if not provided, agent generates fallback) */
  summary?: string;
}

/**
 * MockMessage represents a message emitted by the mock driver's query stream.
 * Compatible with the message format expected by agent.ts runClaude() function.
 */
export interface MockMessage {
  /** Message type: assistant (working) or result (final outcome) */
  type: "assistant" | "result";
  /** Assistant message with content blocks (text, tool_use) */
  message?: { content: Array<{ type: string; text?: string; name?: string }> };
  /** Result subtype (e.g., "success", "error") */
  subtype?: string;
  /** Whether the result indicates an error */
  is_error?: boolean;
  /** Result text content */
  result?: string;
  /** Error messages (if any) */
  errors?: string[];
}
