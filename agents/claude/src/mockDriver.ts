/**
 * Mock Driver for Claude SDK
 *
 * This module provides a deterministic mock implementation of the Claude SDK's
 * query() function for E2E testing without requiring Anthropic API credentials.
 *
 * The mock driver reads a fixture file (specified via HOLON_CLAUDE_MOCK_FIXTURE)
 * that describes file operations to perform and the expected outcome.
 */

import fs from "fs";
import path from "path";
import { spawnSync } from "child_process";
import type { MockFixture, MockOperation, MockMessage } from "./types.js";
import { resolveRuntimePaths } from "./runtimePaths.js";

/**
 * Mock implementation of Claude SDK's query() function.
 *
 * Emits a stream of messages compatible with agent.ts's runClaude() parsing logic:
 * - First: an "assistant" message with tool_use blocks
 * - Second: a "result" message with success/failure status
 *
 * @param params - Query parameters (prompt and options)
 * @returns AsyncIterable of mock messages
 */
export async function* query(
  params: { prompt: string; options?: any }
): AsyncIterable<MockMessage> {
  const fixturePath = process.env.HOLON_CLAUDE_MOCK_FIXTURE;
  if (!fixturePath) {
    throw new Error(
      "HOLON_CLAUDE_MOCK_FIXTURE must be set in mock mode. " +
        'Example: export HOLON_CLAUDE_MOCK_FIXTURE=/path/to/fixture.json'
    );
  }

  // Load and validate fixture file
  let fixture: MockFixture;
  try {
    const fixtureContent = fs.readFileSync(fixturePath, "utf8");
    fixture = JSON.parse(fixtureContent) as MockFixture;
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    throw new Error(
      `Failed to load mock fixture from ${fixturePath}: ${message}`
    );
  }

  if (fixture.version !== "v1") {
    throw new Error(
      `Unsupported mock fixture version: ${fixture.version}. Expected "v1".`
    );
  }

  // Emit assistant message (simulates Claude working on the task)
  yield {
    type: "assistant",
    message: {
      content: [
        { type: "text", text: "I'll help you with this task." },
        { type: "tool_use", name: "Edit" },
      ],
    },
  };

  const runtimePaths = resolveRuntimePaths(process.env);
  const workspacePath = params.options?.cwd || runtimePaths.workspaceDir;

  // Execute all file operations deterministically
  // Filter out write_output operations which are handled separately below
  for (const op of fixture.operations) {
    if (op.type === "write_output") {
      // These are handled separately in the output artifacts loop below
      continue;
    }
    executeOperation(op, workspacePath);
  }

  // Stage changes with git (critical for diff.patch generation)
  // This replicates the exact sequence from agent.ts lines 559, 565-566
  const addResult = spawnSync("git", ["add", "-A"], {
    cwd: workspacePath,
    stdio: "pipe",
  });
  if (addResult.status !== 0) {
    const stderr = addResult.stderr?.toString().trim();
    console.warn(
      `[Mock Driver] Git staging failed: "git add -A" exited with code ${addResult.status}` +
      (stderr ? `, stderr: ${stderr}` : "")
    );
  }

  const resetHolonResult = spawnSync("git", ["reset", "holon"], {
    cwd: workspacePath,
    stdio: "pipe",
  });
  if (resetHolonResult.status !== 0) {
    const stderr = resetHolonResult.stderr?.toString().trim();
    console.warn(
      `[Mock Driver] Git staging failed: "git reset holon" exited with code ${resetHolonResult.status}` +
      (stderr ? `, stderr: ${stderr}` : "")
    );
  }

  const resetBinHolonResult = spawnSync("git", ["reset", "bin/holon"], {
    cwd: workspacePath,
    stdio: "pipe",
  });
  if (resetBinHolonResult.status !== 0) {
    const stderr = resetBinHolonResult.stderr?.toString().trim();
    console.warn(
      `[Mock Driver] Git staging failed: "git reset bin/holon" exited with code ${resetBinHolonResult.status}` +
      (stderr ? `, stderr: ${stderr}` : "")
    );
  }

  // Write output artifacts (summary.md, pr-fix.json, etc.)
  for (const op of fixture.operations) {
    if (op.type === "write_output") {
      const outputPath = path.join(runtimePaths.outputDir, op.path);
      const outputDir = path.dirname(outputPath);

      try {
        fs.mkdirSync(outputDir, { recursive: true });
        fs.writeFileSync(outputPath, op.content || "");
      } catch (err: unknown) {
        const message = err instanceof Error ? err.message : String(err);
        throw new Error(`Failed to write output artifact ${outputPath}: ${message}`);
      }
    }
  }

  // Write summary.md if provided in fixture
  if (fixture.outcome.summary) {
    const summaryPath = path.join(runtimePaths.outputDir, "summary.md");
    try {
      fs.writeFileSync(summaryPath, fixture.outcome.summary);
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      throw new Error(`Failed to write summary.md: ${message}`);
    }
  }

  // Emit final result message
  yield {
    type: "result",
    subtype: fixture.outcome.success ? "success" : "error",
    is_error: !fixture.outcome.success,
    result: fixture.outcome.result_text,
  };
}

/**
 * Execute a single mock file operation.
 *
 * @param op - Operation to execute (excluding write_output which is handled separately)
 * @param workspacePath - Base workspace directory
 */
function executeOperation(op: Exclude<MockOperation, { type: "write_output" }>, workspacePath: string): void {

  // Normalize workspace path once to an absolute base directory
  const baseDir = path.resolve(workspacePath);
  const targetPath = path.join(baseDir, op.path);

  // Security check: prevent path traversal attacks
  const resolved = path.normalize(targetPath);
  const baseWithSep = baseDir.endsWith(path.sep) ? baseDir : baseDir + path.sep;
  if (resolved !== baseDir && !resolved.startsWith(baseWithSep)) {
    throw new Error(
      `Path traversal detected in mock fixture: ${op.path} ` +
        `(resolves to ${resolved}, outside workspace ${baseDir})`
    );
  }

  switch (op.type) {
    case "write_file":
      try {
        fs.mkdirSync(path.dirname(targetPath), { recursive: true });
        fs.writeFileSync(targetPath, op.content || "");
      } catch (err: unknown) {
        const message = err instanceof Error ? err.message : String(err);
        throw new Error(
          `Failed to write file ${targetPath} in mock operation: ${message}`
        );
      }
      break;

    case "append_file":
      try {
        fs.mkdirSync(path.dirname(targetPath), { recursive: true });
        fs.appendFileSync(targetPath, op.content || "");
      } catch (err: unknown) {
        const message = err instanceof Error ? err.message : String(err);
        throw new Error(
          `Failed to append to file ${targetPath} in mock operation: ${message}`
        );
      }
      break;

    case "delete_file":
      // Check if file exists before deleting
      if (!fs.existsSync(targetPath)) {
        console.warn(`[Mock Driver] File does not exist, skipping delete: ${targetPath}`);
        break;
      }
      try {
        fs.unlinkSync(targetPath);
      } catch (err: unknown) {
        const message = err instanceof Error ? err.message : String(err);
        throw new Error(
          `Failed to delete file ${targetPath} in mock operation: ${message}`
        );
      }
      break;

    default: {
      // TypeScript exhaustiveness check
      // After filtering out write_output, only these 3 types remain
      const _exhaustive: never = op as never;
      throw new Error(`Unsupported operation type: ${_exhaustive}`);
    }
  }
}
